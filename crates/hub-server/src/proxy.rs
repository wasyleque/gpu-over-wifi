use std::time::Duration;

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Request, State},
    http::{header::HeaderName, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use serde_json::json;
use tracing::warn;

use crate::{verify_bearer_token, AppState};

/// Domyślny limit rozmiaru body żądania w MiB.
pub const DEFAULT_MAX_BODY_MIB: usize = 64;

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

async fn proxy_handler(State(state): State<AppState>, req: Request) -> Response {
    if !verify_bearer_token(req.headers(), &state.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let (parts, body) = req.into_parts();

    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or_else(|| parts.uri.path());

    let target_url = format!("{}{}", state.ollama_url.trim_end_matches('/'), path_and_query);

    let req_conn_scoped = connection_scoped_headers(&parts.headers);
    let mut forwarded_headers = HeaderMap::new();
    for (name, value) in &parts.headers {
        if !should_skip_request_header(name, &req_conn_scoped) {
            forwarded_headers.append(name.clone(), value.clone());
        }
    }

    let reqwest_body = reqwest::Body::wrap_stream(body.into_data_stream());

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

    let stream = upstream_resp.bytes_stream();
    response_builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_router, DEFAULT_PORT};
    use axum::{
        body::Bytes,
        http::{HeaderValue, Method},
        routing::post,
    };
    use http_body_util::BodyExt;
    use std::convert::Infallible;
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    fn make_state(ollama_url: String) -> AppState {
        AppState {
            token: "secret-test-token".to_string(),
            hostname: "test-srv".to_string(),
            port: DEFAULT_PORT,
            ollama_url,
            http_client: reqwest::Client::builder()
                .timeout(Duration::from_millis(200))
                .build()
                .unwrap(),
            proxy_client: build_proxy_client(),
            max_body_mib: DEFAULT_MAX_BODY_MIB,
            nvml: None,
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
        let mock_backend = Router::new().route(
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
}
