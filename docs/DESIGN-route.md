# Projekt: trasa aplikacji (karta lokalna albo zdalna)

Status: wdrożone w `gpu-icd` 0.3.11 jako opcja (`GPU_ICD_ROUTE`), domyślnie wyłączone. Kod: `crates/gpu-icd/src/route.rs`, narzędzie `gpu-route`.

## 1. Pomysł i co z niego zostało po pomiarach
Pomysł wyjściowy ("akcelerator jak Voodoo"): karta lokalna laptopa robi to, co potrafi, a zdalna RTX bierze tylko to, co dla lokalnej za trudne.

Dwie wersje tego pomysłu odpadają na danych, które już mamy:
- **Podział pracy w obrębie jednej aplikacji** (część rysowań lokalnie, część zdalnie) jest niewykonalny: zasoby Vulkana (obrazy, bufory, potoki) żyją w jednym
  urządzeniu. Każdy zasób używany po obu stronach trzeba by utrzymywać dwa razy i synchronizować, a ciężkie sceny są już ograniczone łączem
  (85-96 MB/s na kablu 1 Gb/s, kodowanie przy granicy: przebiegi 28-32 w `docs/measurements/2026-11-05-rottr/README.md`). Podział zwiększyłby ruch, nie zmniejszył.
- **Przenoszenie działającej aplikacji między kartami** odpada z tego samego powodu (stan urządzenia nie jest przenośny).

Zostaje jednostka, którą da się przełączać bez kosztu: **cały proces**. Lekka aplikacja idzie w całości na kartę lokalną (zero sieci, zero obiegów, zero
opóźnienia obrazu, serwer wolny), ciężka w całości na zdalną. To dokładnie podział z analogii: karta 2D obsługuje pulpit, akcelerator tylko gry 3D.
Druga korzyść jest niezależna od wydajności: gdy serwer nie odpowiada, aplikacja i tak startuje (dotąd: `Found no drivers!`).

Konsultacja zewnętrzna (agy, 2026-11-11) doszła do tych samych zasad od strony ogólnej: decyzja z fizyki i pomiaru, nie ze zgadywania "trudności";
logika trasy poza warstwą transportu (`gpu-renderd` i protokół bez zmian); trasa wg trudności nie ma sensu dla inferencji LLM (tam zawsze wygrywa zdalna karta).

## 2. Mechanizm: trampolina przy negocjacji ICD
Loader woła trzy eksporty ICD: `vk_icdNegotiateLoaderICDInterfaceVersion`, `vk_icdGetInstanceProcAddr`, `vk_icdGetPhysicalDeviceProcAddr`.
Przy pierwszym z nich zapada decyzja (raz na proces). Dla trasy lokalnej wszystkie trzy oddają odpowiedzi lokalnego sterownika (ładowanego
bezpośrednio z jego manifestu, jak przy prezentacji w `local.rs`): aplikacja dostaje jego wskaźniki funkcji i jego uchwyty, więc po starcie
nasz kod nie leży na żadnej ścieżce wywołań. Wyjątek: `vkQueuePresentKHR` (przez podmienione `vkGetDeviceProcAddr`) przechodzi przez licznik klatek.
Dla trasy zdalnej nic się nie zmienia poza jednym wywołaniem licznika w `swapchain::queue_present`.

## 3. Miara: czas klatki ważony czasem gry
Porównywanie średnich FPS jest mylące (menu zawyża, AGENTS.md 2026-11-06). Zbieramy histogram odstępów między `vkQueuePresentKHR`:
- przedziały co 1/8 oktawy od 0,5 ms do ok. 2 s (błąd odczytu percentyla do 4,5%, poniżej rozrzutu pomiarów ±6%);
- w przedziale sumujemy **czas**, nie liczbę klatek: minuta menu przy 300 FPS waży tyle, co minuta sceny przy 20 FPS;
- pomijane: pierwsze 5 s (shadery, intro) i odstępy powyżej 1 s (pauza, ładowanie);
- decyduje **75. percentyl** (czas klatki, poniżej którego mieści się 3/4 czasu gry), czyli zachowanie w cięższych scenach.

Histogram sumuje się między uruchomieniami w pliku `~/.local/state/gpu-hub/route/<klucz>.route` (osobny wiersz dla każdej trasy; zapis co 15 s,
bo gry bywają zabijane; powyżej 4 h danych histogram jest połowiony, żeby nowe pomiary miały wagę). Uruchomienie krótsze niż 60 s gry i 600 klatek
nie jest zapisywane i nie decyduje. Pomiar trasy zdalnej jest ważny tylko dla tej samej wersji `gpu-icd` (jej wydajność zmienia się z każdą
optymalizacją: 10 → 26 FPS w tydzień), pomiar lokalnej nie wygasa.

Klucz profilu: `GPU_ICD_ROUTE_KEY`, potem `steam-<SteamAppId>`, potem nazwa pliku z wiersza poleceń (pod Wine: nazwa pliku gry), potem nazwa procesu.

## 4. Decyzja (`GPU_ICD_ROUTE=auto`)
Kolejność, pierwsza pasująca reguła wygrywa:

| Warunek | Trasa | Powód w logu |
|---|---|---|
| brak lokalnego sterownika | zdalna | `NoLocalDriver` |
| serwer nie przyjmuje połączeń (sonda TCP, 300 ms) albo brak konfiguracji | lokalna | `ServerUnreachable` |
| brak pomiaru lokalnej | lokalna | `ExploreLocal` |
| lokalna spełnia cel (`GPU_ICD_ROUTE_TARGET_FPS`, domyślnie 60) | lokalna | `LocalMeetsTarget` |
| lokalna za wolna, brak ważnego pomiaru zdalnej | zdalna | `ExploreRemote` |
| zdalna szybsza o co najmniej 10% | zdalna | `RemoteFaster` |
| w pozostałych przypadkach | lokalna | `LocalNotSlower` |

Uzasadnienie kolejności: karta lokalna jest tańsza pod każdym względem poza mocą, więc zaczynamy od niej i zdalną bierzemy dopiero wtedy, gdy pomiar
pokaże, że lokalna nie wystarcza. Próg 10% pokrywa rozrzut pomiarów i to, że trasa zdalna dokłada opóźnienie obrazu, którego FPS nie widzi.
Koszt nauki: jedno uruchomienie na trasę (ciężka gra raz zagra wolno lokalnie; `GPU_ICD_ROUTE=remote` pomija ten krok).

`GPU_ICD_ROUTE=local` i `remote` wymuszają trasę (i też zbierają pomiar). Bez zmiennej moduł jest wyłączony i sterownik zachowuje się jak dotąd.
Każdy start wypisuje jedną linię: `[gpu-icd] trasa: Local (LocalMeetsTarget), profil ...: lokalnie 50.3 FPS, zdalnie brak pomiaru, cel 40 FPS`.

## 5. Sprawdzone i niesprawdzone
Sprawdzone (laptop z Intel UHD 620, 2026-11-11):
- 13 testów jednostkowych (decyzja, percentyle ważone czasem, rozgrzewka i pauzy, profil, wygasanie wersji, klucze, trampolina na atrapie sterownika);
- przez nasz manifest ICD: `vulkaninfo` z `GPU_ICD_ROUTE=local` pokazuje kartę Intela; `auto` bez serwera wybiera lokalną zamiast błędu;
- `gpu-smoke swapchain` (powierzchnia headless, `GPU_SMOKE_FRAMES=3500 GPU_SMOKE_FRAME_MS=20`) na prawdziwym sterowniku Mesa: 3500 klatek przez
  trampolinę, profil zapisany, p75 19,9 ms (oczekiwane 20 ms); `auto` z tym profilem: cel 40 FPS → `LocalMeetsTarget`, cel 60 FPS → `ExploreRemote`.

Niesprawdzone (serwer `gpu-renderd` nieosiągalny z tego laptopa):
- zapis pomiaru na trasie zdalnej w prawdziwej grze (ten sam licznik, jedno wywołanie w `queue_present`);
- gra pod Proton/DXVK na trasie lokalnej (procesy pomocnicze Wine, kontener pressure-vessel);
- sonda TCP zostawia na serwerze połączenie zamknięte przed logowaniem: sprawdzić, czy demon nie loguje tego jako błędu.

## 6. Czego świadomie nie ma
- Trasy wg obciążenia serwera (kolejka, VRAM): dziś jeden klient na serwer; gdy będzie ich więcej, `remote_ok` można rozszerzyć o `/status` huba.
- Uwzględnienia opóźnienia obrazu z `latency.rs` w mierze (na razie zastępuje je margines 10%).
- Wyboru między kilkoma lokalnymi kartami (bierzemy pierwszą wg kolejności z `local.rs`: Intel, AMD, NVIDIA).
