# Podłączanie klientów do GPU Hub

Hub (`hub-server`, port 7878) wystawia API zgodne z OpenAI pod `http://<serwer>:7878/v1` oraz natywne API Ollamy pod `/api`. Każde zapytanie wymaga tokenu (`Authorization: Bearer <token>`). Dostęp tylko z LAN lub przez VPN.

## 1. hubctl

```sh
export HUB_SERVER=http://192.168.0.11:7878
export HUB_TOKEN=...            # token z serwera: ~/gpu-hub/token

hubctl status                   # GPU, załadowane modele, stan schedulera
hubctl models                   # modele dostępne przez hub
eval "$(hubctl env)"            # ustawia OPENAI_BASE_URL / OPENAI_API_BASE / OPENAI_API_KEY
```

`hubctl env --shell fish|powershell|cmd` drukuje składnię dla innych powłok. Wynik zawiera token — nie zapisuj go do pliku współdzielonego ani logu.

Windows (PowerShell):

```powershell
$env:HUB_SERVER = "http://192.168.0.11:7878"
$env:HUB_TOKEN  = "..."
.\hubctl.exe status
.\hubctl.exe env --shell powershell | Invoke-Expression
```

Binarkę Windows buduje się z Linuksa: `cargo zigbuild --release --target x86_64-pc-windows-gnu -p hub-client` (stan: zbudowana, nieprzetestowana na Windows).

## 2. Narzędzia zgodne z OpenAI

Po `eval "$(hubctl env)"` większość narzędzi działa bez dalszej konfiguracji. Nazwa modelu to tag z `hubctl models`.

| Narzędzie | Użycie |
|---|---|
| **aider** | `aider --model openai/qwen3.8:latest plik.py` (sprawdzone: poprawka błędu w pliku przez hub) |
| **Python (openai)** | `OpenAI(base_url="http://192.168.0.11:7878/v1", api_key=TOKEN)` |
| **curl** | `curl -H "Authorization: Bearer $HUB_TOKEN" $HUB_SERVER/v1/chat/completions -d '{"model":"llama3.2:latest","messages":[...]}'` |
| **Open WebUI** | Ustawienia → Połączenia → OpenAI: URL `http://192.168.0.11:7878/v1`, klucz = token |
| **Continue / Cline / inne** | provider „OpenAI-compatible”, `apiBase` jak wyżej, `apiKey` = token |

## 3. Czego hub nie robi (jeszcze)
- Brak TLS — token idzie otwartym tekstem; używaj wyłącznie w VPN/LAN.
- Jeden wspólny token dla wszystkich klientów; przez `/api/*` można też usuwać i pobierać modele Ollamy.
- Streaming (`"stream": true`) działa; kolejka i przełączanie modeli opisane w `docs/tasks/E4a-scheduler-core.md`.
