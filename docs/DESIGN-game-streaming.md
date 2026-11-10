> **KOREKTA (2026-10-31, po uwadze użytkownika).** Celem NIE jest streaming gry z serwera, tylko **uruchamianie gry na laptopie z Linuksem tak, jakby karta serwera była w nim zamontowana** (gra, CPU, dysk, biblioteka Steam i zapisy zostają lokalnie; renderuje karta serwera). To jest mechanizm **B** poniżej. Mechanizm A (Sunshine/Moonlight, gra działa na serwerze) był moim skrótem i został **zdegradowany do opcji awaryjnej**: etapy E5a–E5c zostają w repo jako plan B i **nie są wykonywane**. Głównym torem jest teraz **E5-VK** (`docs/tasks/E5-VK-remote-venus-poc.md`). Przyczyna pomyłki: wybrałem A, bo był gotowy i szybki, i nie sprawdziłem tego z użytkownikiem.

# Projekt: GPU serwera do gier po sieci (laptop jako pierwszy klient)

Cel użytkownika: używać karty graficznej serwera tak, jakby była zamontowana w laptopie. Pierwszy klient: ten laptop (Omarchy/Arch, Intel HD 520, ekran 1366×768, `moonlight-qt` 6.2.0 zainstalowany).

## 1. Dwa różne mechanizmy (i dlaczego nie są tym samym)

| | A. Strumieniowanie obrazu | B. Zdalne wywołania API grafiki ("wirtualny GPU") |
|---|---|---|
| Gdzie działa gra | **na serwerze** | **na laptopie** |
| Co płynie przez sieć | skompresowane wideo + wejście | polecenia Vulkan/DX/GL + dane (rzędu setek Mb/s) |
| Gotowe narzędzia | Sunshine + Moonlight | brak utrzymywanych otwartych (istnieją tylko komercyjne, zamknięte) |
| Wysiłek | dni | miesiące–lata (sterownik/warstwa Vulkan, predykcja, kompresja) |
| Opóźnienie | ~20–35 ms (koniec-do-końca) | zależy od sprytu protokołu; RTT sieci zabija naiwną wersję |

Dla **gier** oba dają użytkownikowi ten sam efekt: gra korzysta z karty serwera. B ma sens dopiero dla aplikacji, które muszą działać lokalnie, ale liczyć na zdalnym GPU (CUDA, Blender) — to osobny etap (E7). **Decyzja: gry realizujemy mechanizmem A.** B zostaje badaniem (E5d).

## 2. Decyzje projektowe (podjęte, bez pytań)

1. **Serwer do gier: nowy serwer `gpu-hub` (RTX 3090 Ti, 10.42.0.2), nie `.11`.** Połączony kablem bezpośrednio z laptopem: RTT 0,55 ms. `.11` jest za VPN — opóźnienie i jitter wykluczają gry.
2. **Stos serwera:** Xorg (NVIDIA, wirtualny monitor) + openbox + PipeWire (wirtualne wyjście dźwięku) + Sunshine (kodowanie NVENC) + Steam z Proton. Wszystko jako usługi użytkownika/systemd, zarządzane przez hub.
3. **Kodek: H.264 domyślnie, HEVC opcjonalnie.** RTX 3090 Ti (Ampere) koduje H.264/HEVC, **nie koduje AV1**. HD 520 dekoduje H.264 i HEVC Main sprzętowo (sprawdzone `vainfo`). H.264 daje najmniejsze opóźnienie dekodowania na słabym iGPU.
4. **Rozdzielczość:** klient prosi o 1366×768@60 (natywna laptopa); serwer ustawia tryb X przed sesją (hook `prep-cmd` Sunshine). Możliwy też tryb 1920×1080 z downscalingiem.
5. **Bitrate:** 20–40 Mb/s na kablu (limit łącza 1 Gb/s).
6. **Współdzielenie GPU z AI:** sesja gry to *rezerwacja* w schedulerze (patrz §4). InferSSD (zajmuje całą kartę i RAM) jest wykluczony na czas gry; małe modele Ollamy mogą współistnieć w limicie VRAM.
7. **Bezpieczeństwo:** porty Sunshine otwarte w `ufw` wyłącznie dla `10.42.0.0/24`; parowanie PIN-em; interfejs WWW Sunshine (47990) tylko z łącza bezpośredniego.

## 3. Licencje (obowiązkowe przed publikacją)
- **Sunshine: GPL-3.0**, **Moonlight: GPL-3.0**. Hub uruchamia je wyłącznie jako **osobne programy** (instalowane z oficjalnych paczek), nie dołącza ich kodu ani binarek do repozytorium i nie linkuje. Dokumentacja podaje tylko, jak je zainstalować.
- Steam/Proton: oprogramowanie Valve, instalowane z repozytorium dystrybucji; hub niczego nie redystrybuuje.
- Paczkę Sunshine instalujemy z wydania GitHub z **weryfikacją sumy SHA-256**, nie przez `curl | sh`.

## 4. Integracja z hubem

### 4.1 Sesja gry jako rezerwacja
Scheduler dostaje pojęcie **rezerwacji GPU** niezależnej od żądań HTTP: `Reservation { owner: "game", vram_mib, exclusive_ram: bool }`.
- Start sesji: drain + `unload_all` InferSSD; limit VRAM Ollamy zmniejszony o `game_vram_reserve_mib` (domyślnie 12000); odrzucanie/kolejkowanie żądań do InferSSD z kodem 503 "GPU zajęte przez grę".
- Koniec sesji: zwolnienie rezerwacji, powrót do normalnego trybu.
- **Wykrywanie bezczynności bez haseł:** NVML udostępnia liczbę aktywnych sesji kodera (NVENC). Brak sesji przez `game_idle_secs` (domyślnie 300) = koniec sesji. Nie potrzeba dostępu do API Sunshine.

### 4.2 API huba
- `POST /v1/game/session` (start; uruchamia usługi X/Sunshine, czeka na gotowość, zwraca host/port/aplikacje),
- `GET /v1/game/session` (stan, liczba sesji NVENC, zajęty VRAM),
- `DELETE /v1/game/session` (stop).
Autoryzacja tym samym tokenem. Typy w `hub-proto`.

### 4.3 Klient: `hubctl game`
- `hubctl game start [--app Desktop|Steam] [--res auto|WxH] [--fps 60] [--codec h264|hevc]` → woła API, czeka na gotowość, uruchamia `moonlight stream <host> <app> ...`; po wyjściu z Moonlighta kończy sesję (lub zostawia z timeoutem bezczynności).
- `hubctl game stop|status|pair`.
- Opcjonalnie skrót klawiszowy Hyprland (`~/.config/hypr/bindings.lua`) uruchamiający `hubctl game start`.
- Parowanie (jednorazowe): `moonlight pair <host>` + PIN w interfejsie Sunshine; automatyzacja po wdrożeniu przechowywania poświadczeń (poza zakresem pierwszej wersji).

## 5. Etapy (małe, weryfikowalne)
- **E5a Provisioning serwera** (wymaga roota, skrypt uruchamia użytkownik): `docs/tasks/E5a-server-provisioning.md`.
- **E5b Rezerwacje GPU + API sesji gry w hub-server**: `docs/tasks/E5b-game-session-api.md`.
- **E5c `hubctl game`** + integracja z Hyprland: `docs/tasks/E5c-hubctl-game.md`.
- **E5d Badanie B (zdalne Vulkan)** — spike 1–2 dni: warstwa tracingu `VK_LAYER` mierzy strumień poleceń prostej aplikacji (liczba bajtów/s i rund-trip); decyzja go/no-go na podstawie liczb, nie intuicji.

## 6. Ryzyka i czego nie wiemy
- Xorg + NVIDIA w trybie bez monitora (virtual display) bywa kapryśny: konfiguracja `ConnectedMonitor`/EDID wymaga walidacji na tym sprzęcie. To główne ryzyko E5a.
- Sunshine na Ubuntu 26.04 + sterownik 580 + jądro 7.0: oficjalna paczka istnieje, ale **nie sprawdziliśmy jeszcze, czy działa** na tej kombinacji.
- Gry z anty-cheatem jądra (np. część gier sieciowych) nie działają pod Proton; wtedy potrzebny byłby Windows na serwerze.
- Opóźnienie koniec-do-końca (cel ~20–35 ms) trzeba **zmierzyć** (nakładka statystyk Moonlighta), nie zakładać.
- Internet serwera idzie przez Wi-Fi laptopa (NAT): pobieranie gier Steam obciąża to łącze.
