use std::time::Duration;

use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Request, State},
    http::{header::HeaderName, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tracing::warn;

use crate::scheduler::{LeaseGuard, ScheduleError};
use crate::{verify_bearer_token, AppState};

/// Domyślny limit rozmiaru body żądania w MiB.
pub const DEFAULT_MAX_BODY_MIB: usize = 64;

/// Maksymalny rozmiar prefiksu body buforowanego na potrzeby odczytu pola `model` (1 MiB).
const MODEL_INSPECTION_LIMIT_BYTES: usize = 1024 * 1024;

/// Tworzy klienta HTTP dla proxy: timeout tylko na nawiązanie połączenia (5 s),
/// brak ogólnego timeoutu na odpowiedź (wymagane dla streamingu SSE/NDJSON).
pub fn build_proxy_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()
        .expect("Błąd inicjalizacji klienta HTTP proxy")
}

/// Dodaje trasy reverse proxy (`/v1/*` i `/api/*`) do routera.
pub fn register_routes(router: Router<AppState>, max_body_mib: usize) -> Router<AppState> {
    let limit_bytes = max_body_mib.saturating_mul(1024 * 1024);
    let proxy_router = Router::new()
        .route("/v1/*path", any(proxy_handler))
        .route("/api/*path", any(proxy_handler))
        .layer(DefaultBodyLimit::max(limit_bytes));

    router.merge(proxy_router)
}

/// Sprawdza czy dana ścieżka jest trasą generacji/embeddingów wymagającą sprawdzenia pola `model`.
pub fn is_model_routed_path(path: &str) -> bool {
    matches!(
        path,
        "/v1/chat/completions"
            | "/v1/completions"
            | "/v1/embeddings"
            | "/api/chat"
            | "/api/generate"
    ) || path.starts_with("/api/embed")
}

#[derive(Deserialize)]
struct ModelFieldOnly {
    model: Option<String>,
}

/// Próbuje odczytać pole `model` z prefiksu JSON.
pub fn extract_model_from_json(bytes: &[u8]) -> Option<String> {
    let parsed: ModelFieldOnly = serde_json::from_slice(bytes).ok()?;
    parsed.model.filter(|m| !m.trim().is_empty())
}

fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn connection_scoped_headers(headers: &HeaderMap) -> Vec<HeaderName> {
    headers
        .get_all(axum::http::header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(','))
        .filter_map(|tok| HeaderName::from_bytes(tok.trim().as_bytes()).ok())
        .collect()
}

fn should_skip_request_header(name: &HeaderName, conn_scoped: &[HeaderName]) -> bool {
    is_hop_by_hop(name)
        || name == axum::http::header::HOST
        || name.as_str().eq_ignore_ascii_case(hub_proto::AUTH_HEADER)
        || conn_scoped.iter().any(|h| h == name)
}

fn should_skip_response_header(name: &HeaderName, conn_scoped: &[HeaderName]) -> bool {
    is_hop_by_hop(name) || conn_scoped.iter().any(|h| h == name)
}

fn backend_unavailable_response() -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({ "error": "backend unavailable" })),
    )
        .into_response()
}

fn unknown_model_response() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "unknown model" })),
    )
        .into_response()
}

/// Buforuje maksymalnie `limit` bajtów ze strumienia body i zwraca zbuforowany prefiks
/// oraz odtworzony strumień całego body (prefiks + reszta strumienia bez buforowania całości).
async fn peek_body_prefix(
    body: Body,
    limit: usize,
) -> Result<(Bytes, reqwest::Body), axum::Error> {
    let mut data_stream = body.into_data_stream();
    let mut buffered = Vec::new();
    let mut first_overflow_chunk: Option<Bytes> = None;

    while let Some(chunk_res) = data_stream.next().await {
        let chunk = chunk_res?;
        if buffered.len() + chunk.len() <= limit {
            buffered.extend_from_slice(&chunk);
            if buffered.len() == limit {
                break;
            }
        } else {
            let needed = limit - buffered.len();
            buffered.extend_from_slice(&chunk[..needed]);
            first_overflow_chunk = Some(chunk.slice(needed..));
            break;
        }
    }

    let prefix_bytes = Bytes::from(buffered);
    let prefix_for_stream = prefix_bytes.clone();

    let combined = futures_util::stream::once(async move {
        Ok::<Bytes, axum::Error>(prefix_for_stream)
    })
    .chain(futures_util::stream::iter(
        first_overflow_chunk.map(Ok::<Bytes, axum::Error>),
    ))
    .chain(data_stream);

    Ok((prefix_bytes, reqwest::Body::wrap_stream(combined)))
}

/// `GET /v1/models`: lista z domyślnego backendu (Ollama) uzupełniona o modele pozostałych backendów.
async fn merged_models_response(state: &AppState) -> Response {
    let url = format!(
        "{}/v1/models",
        state.scheduler.default_backend_url().trim_end_matches('/')
    );
    let upstream = match state.http_client.get(&url).send().await {
        Ok(r) if r.status().is_success() => r.json::<serde_json::Value>().await.ok(),
        _ => None,
    };
    let extra = state.scheduler.extra_model_ids().await;
    if upstream.is_none() && extra.is_empty() {
        return backend_unavailable_response();
    }
    let mut body = upstream.unwrap_or_else(|| json!({ "object": "list", "data": [] }));
    if let Some(data) = body.get_mut("data").and_then(|d| d.as_array_mut()) {
        for id in extra {
            data.push(json!({ "id": id, "object": "model", "created": 0, "owned_by": "hub" }));
        }
    }
    (StatusCode::OK, Json(body)).into_response()
}

async fn proxy_handler(State(state): State<AppState>, req: Request) -> Response {
    if !verify_bearer_token(req.headers(), &state.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    if req.method() == axum::http::Method::GET && req.uri().path() == "/v1/models" {
        return merged_models_response(&state).await;
    }

    let (parts, body) = req.into_parts();
    let path = parts.uri.path();

    let (lease_guard, target_base_url, reqwest_body): (Option<LeaseGuard>, String, reqwest::Body) =
        if is_model_routed_path(path) {
            let (prefix, reassembled_body) =
                match peek_body_prefix(body, MODEL_INSPECTION_LIMIT_BYTES).await {
                    Ok(res) => res,
                    Err(e) => {
                        warn!("Błąd odczytu body żądania: {e}");
                        return StatusCode::BAD_REQUEST.into_response();
                    }
                };

            if let Some(model_name) = extract_model_from_json(&prefix) {
                match state.scheduler.acquire(&model_name).await {
                    Ok(lease) => {
                        let url = lease.base_url().to_string();
                        (Some(lease), url, reassembled_body)
                    }
                    Err(ScheduleError::UnknownModel(_)) => {
                        return unknown_model_response();
                    }
                    Err(ScheduleError::BackendError(_)) => {
                        return backend_unavailable_response();
                    }
                    Err(ScheduleError::Cancelled) => {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                }
            } else {
                // Brak pola `model` -> przekaż do domyślnego backendu bez kolejkowania
                (
                    None,
                    state.scheduler.default_backend_url(),
                    reassembled_body,
                )
            }
        } else {
            // Trasa bez modelu -> przekaż do domyślnego backendu bez kolejkowania
            let reqwest_body = reqwest::Body::wrap_stream(body.into_data_stream());
            (None, state.scheduler.default_backend_url(), reqwest_body)
        };

    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or_else(|| parts.uri.path());

    let target_url = format!("{}{}", target_base_url.trim_end_matches('/'), path_and_query);

    let req_conn_scoped = connection_scoped_headers(&parts.headers);
    let mut forwarded_headers = HeaderMap::new();
    for (name, value) in &parts.headers {
        if !should_skip_request_header(name, &req_conn_scoped) {
            forwarded_headers.append(name.clone(), value.clone());
        }
    }

    let upstream_req = state
        .proxy_client
        .request(parts.method, &target_url)
        .headers(forwarded_headers)
        .body(reqwest_body);

    let upstream_resp = match upstream_req.send().await {
        Ok(resp) => resp,
        Err(e) => {
            warn!("Błąd połączenia z backendem AI ({target_url}): {e}");
            return backend_unavailable_response();
        }
    };

    let status = upstream_resp.status();
    let upstream_headers = upstream_resp.headers().clone();
    let resp_conn_scoped = connection_scoped_headers(&upstream_headers);

    let mut response_builder = Response::builder().status(status);
    if let Some(resp_headers) = response_builder.headers_mut() {
        for (name, value) in &upstream_headers {
            if !should_skip_response_header(name, &resp_conn_scoped) {
                resp_headers.append(name.clone(), value.clone());
            }
        }
    }

    // Utrzymujemy `lease_guard` przy życiu przez cały czas trwania strumienia odpowiedzi
    let stream = futures_util::stream::unfold(
        (upstream_resp.bytes_stream(), lease_guard),
        |(mut s, guard)| async move {
            s.next().await.map(|item| (item, (s, guard)))
        },
    );

    response_builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Backend, OllamaBackend};
    use crate::scheduler::{Scheduler, SchedulerConfig};
    use crate::{build_router, DEFAULT_PORT};
    use axum::{
        body::Bytes,
        http::{HeaderValue, Method},
        routing::{get, post},
    };
    use http_body_util::BodyExt;
    use std::convert::Infallible;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    fn make_state(ollama_url: String) -> AppState {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let backend: Arc<dyn Backend> = Arc::new(OllamaBackend::with_name_and_models(
            "ollama".to_string(),
            ollama_url.clone(),
            http_client.clone(),
            vec!["qwen2.5".to_string(), "qwen".to_string()],
        ));
        let scheduler = Scheduler::new(SchedulerConfig::default(), vec![backend]);
        AppState {
            token: "secret-test-token".to_string(),
            hostname: "test-srv".to_string(),
            port: DEFAULT_PORT,
            ollama_url,
            http_client,
            proxy_client: build_proxy_client(),
            max_body_mib: DEFAULT_MAX_BODY_MIB,
            nvml: None,
            scheduler,
        }
    }

    async fn spawn_mock_backend(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn proxy_rejects_missing_or_invalid_token() {
        let state = make_state("http://127.0.0.1:9".to_string());
        let app = build_router(state);

        // Brak tokenu na /v1/models -> 401
        let res_no_token = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res_no_token.status(), StatusCode::UNAUTHORIZED);

        // Zły token na /api/generate -> 401
        let res_bad_token = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/generate")
                    .header(hub_proto::AUTH_HEADER, "Bearer wrong-token")
                    .body(Body::from(r#"{"model":"qwen"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res_bad_token.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn proxy_forwards_body_status_headers_and_strips_authorization() {
        let mock_backend = Router::new()
            .route(
                "/api/tags",
                get(|| async {
                    Json(json!({
                        "models": [{"name": "qwen2.5", "size": 1024}]
                    }))
                }),
            )
            .route(
                "/api/ps",
                get(|| async { Json(json!({ "models": [] })) }),
            )
            .route(
                "/v1/chat/completions",
                post(|headers: HeaderMap, body: Bytes| async move {
                    // Upewniamy się, że Authorization NIE dotarł do backendu
                    assert!(
                        headers.get(hub_proto::AUTH_HEADER).is_none(),
                        "Nagłówek Authorization nie powinien być przekazywany do Ollamy"
                    );
                    // Sprawdzamy, że niestandardowy nagłówek dotarł
                    let custom = headers
                        .get("x-custom-client")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();

                    let mut resp_headers = HeaderMap::new();
                    resp_headers.insert("x-echoed-custom", HeaderValue::from_str(&custom).unwrap());
                    (StatusCode::CREATED, resp_headers, body)
                }),
            );

        let backend_url = spawn_mock_backend(mock_backend).await;
        let app = build_router(make_state(backend_url));

        let payload = r#"{"model":"qwen2.5","messages":[{"role":"user","content":"hi"}]}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header(hub_proto::AUTH_HEADER, "Bearer secret-test-token")
                    .header("x-custom-client", "aider")
                    .header("content-type", "application/json")
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(
            resp.headers()
                .get("x-echoed-custom")
                .and_then(|v| v.to_str().ok()),
            Some("aider")
        );
        let resp_bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&resp_bytes[..], payload.as_bytes());
    }

    #[tokio::test]
    async fn proxy_streams_chunks_in_order_without_buffering_whole_response() {
        let (chunk_tx, chunk_rx) = mpsc::channel::<Result<Bytes, Infallible>>(8);
        let shared_rx = std::sync::Arc::new(tokio::sync::Mutex::new(Some(chunk_rx)));

        let streaming_backend = Router::new().route(
            "/api/chat",
            post({
                let shared_rx = shared_rx.clone();
                move || {
                    let shared_rx = shared_rx.clone();
                    async move {
                        let rx = shared_rx.lock().await.take().unwrap();
                        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
                            rx.recv().await.map(|item| (item, rx))
                        });
                        Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "application/x-ndjson")
                            .body(Body::from_stream(stream))
                            .unwrap()
                    }
                }
            }),
        );

        let backend_url = spawn_mock_backend(streaming_backend).await;
        let app = build_router(make_state(backend_url));

        let resp_future = tokio::spawn(async move {
            app.oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/chat")
                    .header(hub_proto::AUTH_HEADER, "Bearer secret-test-token")
                    .body(Body::from(r#"{"stream":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap()
        });

        // Wysyłamy pierwszy chunk i sprawdzamy, że klient odbiera go zanim wyślemy kolejne
        chunk_tx
            .send(Ok(Bytes::from("chunk-1\n")))
            .await
            .unwrap();

        let resp = resp_future.await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let mut body_stream = resp.into_body();

        let first_frame = tokio::time::timeout(Duration::from_secs(2), body_stream.frame())
            .await
            .expect("Pierwszy chunk powinien dotrzeć natychmiast (bez buforowania całości)")
            .unwrap()
            .unwrap();
        assert_eq!(first_frame.into_data().unwrap(), Bytes::from("chunk-1\n"));

        // Wysyłamy kolejne dwa chunki i zamykamy strumień
        chunk_tx
            .send(Ok(Bytes::from("chunk-2\n")))
            .await
            .unwrap();
        chunk_tx
            .send(Ok(Bytes::from("chunk-3\n")))
            .await
            .unwrap();
        drop(chunk_tx);

        let mut remaining = Vec::new();
        while let Some(frame_res) = body_stream.frame().await {
            let frame = frame_res.unwrap();
            if let Ok(data) = frame.into_data() {
                remaining.extend_from_slice(&data);
            }
        }
        assert_eq!(remaining, b"chunk-2\nchunk-3\n");
    }

    #[tokio::test]
    async fn proxy_returns_502_when_backend_unavailable() {
        // Bierzemy wolny port i natychmiast zamykamy listener, żeby mieć pewność że jest martwy
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = listener.local_addr().unwrap();
        drop(listener);

        let app = build_router(make_state(format!("http://{dead_addr}")));
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/v1/models")
                    .header(hub_proto::AUTH_HEADER, "Bearer secret-test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(val, json!({ "error": "backend unavailable" }));
    }

    #[tokio::test]
    async fn routing_by_model_and_fallback_without_model() {
        let backend_a = Router::new()
            .route(
                "/api/tags",
                get(|| async {
                    Json(json!({
                        "models": [{"name": "model-a", "size": 1000}]
                    }))
                }),
            )
            .route("/api/ps", get(|| async { Json(json!({ "models": [] })) }))
            .route("/v1/chat/completions", post(|| async { "from-a-chat" }))
            .route("/api/generate", post(|| async { "from-a-generate" }))
            .route(
                "/v1/models",
                get(|| async {
                    Json(json!({
                        "object": "list",
                        "data": [{"id": "model-a", "object": "model"}]
                    }))
                }),
            );

        let backend_b = Router::new()
            .route(
                "/api/tags",
                get(|| async {
                    Json(json!({
                        "models": [{"name": "embed-b", "size": 1000}]
                    }))
                }),
            )
            .route("/api/ps", get(|| async { Json(json!({ "models": [] })) }))
            .route("/api/embeddings", post(|| async { "from-b-embed" }));

        let url_a = spawn_mock_backend(backend_a).await;
        let url_b = spawn_mock_backend(backend_b).await;

        let client = reqwest::Client::new();
        let b1: Arc<dyn Backend> = Arc::new(OllamaBackend::with_name_and_models(
            "ollama-a".to_string(),
            url_a.clone(),
            client.clone(),
            vec![],
        ));
        let b2: Arc<dyn Backend> = Arc::new(OllamaBackend::with_name_and_models(
            "ollama-b".to_string(),
            url_b,
            client.clone(),
            vec![],
        ));
        let scheduler = Scheduler::new(SchedulerConfig::default(), vec![b1, b2]);

        let state = AppState {
            token: "secret-test-token".to_string(),
            hostname: "test-srv".to_string(),
            port: DEFAULT_PORT,
            ollama_url: url_a,
            http_client: client,
            proxy_client: build_proxy_client(),
            max_body_mib: DEFAULT_MAX_BODY_MIB,
            nvml: None,
            scheduler,
        };
        let app = build_router(state);

        // 1. /v1/chat/completions z model-a -> backend A
        let r1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header(hub_proto::AUTH_HEADER, "Bearer secret-test-token")
                    .body(Body::from(r#"{"model":"model-a"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r1.status(), StatusCode::OK);
        let b1_bytes = r1.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&b1_bytes[..], b"from-a-chat");

        // 2. /api/embeddings z embed-b -> backend B
        let r2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/embeddings")
                    .header(hub_proto::AUTH_HEADER, "Bearer secret-test-token")
                    .body(Body::from(r#"{"model":"embed-b"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r2.status(), StatusCode::OK);
        let b2_bytes = r2.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&b2_bytes[..], b"from-b-embed");

        // 3. Fallback bez pola model na trasie /api/generate -> domyślny backend A
        let r3 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/generate")
                    .header(hub_proto::AUTH_HEADER, "Bearer secret-test-token")
                    .body(Body::from(r#"{"prompt":"hello"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r3.status(), StatusCode::OK);
        let b3_bytes = r3.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&b3_bytes[..], b"from-a-generate");

        // 4. /v1/models łączy modele domyślnego backendu A z modelami pozostałych backendów
        let r4 = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/v1/models")
                    .header(hub_proto::AUTH_HEADER, "Bearer secret-test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r4.status(), StatusCode::OK);
        let b4_bytes = r4.into_body().collect().await.unwrap().to_bytes();
        let listed: serde_json::Value = serde_json::from_slice(&b4_bytes).unwrap();
        let ids: Vec<&str> = listed["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["id"].as_str())
            .collect();
        assert_eq!(ids, vec!["model-a", "embed-b"]);
    }

    #[tokio::test]
    async fn unknown_model_returns_404() {
        let mock_backend = Router::new().route(
            "/api/tags",
            get(|| async {
                Json(json!({
                    "models": [{"name": "known-model", "size": 1000}]
                }))
            }),
        );
        let url = spawn_mock_backend(mock_backend).await;
        let client = reqwest::Client::new();
        let b: Arc<dyn Backend> = Arc::new(OllamaBackend::new(url.clone(), client.clone()));
        let scheduler = Scheduler::new(SchedulerConfig::default(), vec![b]);

        let state = AppState {
            token: "secret-test-token".to_string(),
            hostname: "test-srv".to_string(),
            port: DEFAULT_PORT,
            ollama_url: url,
            http_client: client,
            proxy_client: build_proxy_client(),
            max_body_mib: DEFAULT_MAX_BODY_MIB,
            nvml: None,
            scheduler,
        };
        let app = build_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header(hub_proto::AUTH_HEADER, "Bearer secret-test-token")
                    .body(Body::from(r#"{"model":"nonexistent-model"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(val, json!({ "error": "unknown model" }));
    }
}
