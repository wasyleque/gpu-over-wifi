# E5-VK — Zdalny Vulkan: gra na laptopie, renderowanie na karcie serwera (PoC)

Cel użytkownika: gra działa lokalnie na laptopie (Linux), a GPU jest "po sieci jak zamontowane". Mechanizm B z `docs/DESIGN-game-streaming.md`.

## Co już istnieje (sprawdzone 2026-10-31)
- Laptop (Arch): `vulkan-virtio` 26.2.5 w `extra` (sterownik **Venus**, MIT); `socat` zainstalowany. Brak: `vulkan-tools` (`vkcube`).
- Serwer gpu-hub (Ubuntu 26.04): `virgl-server` 1.2.0, `libvirglrenderer1`, `vulkan-tools`, `socat`; w `/usr/share/vulkan/icd.d/` także `virtio_icd.json` i `gfxstream_vk_icd.json`; NVIDIA 580.
- Venus = otwarty protokół serializacji Vulkan (Mesa + virglrenderer). Domyślnie transport to virtio-gpu (maszyny wirtualne). Do testów istnieje transport **vtest** (gniazdo uniksowe) — można go przeprowadzić przez sieć (`socat`).

## Hipotezy do sprawdzenia (nie zakładamy, że działają)
1. Venus przez vtest uruchomi `vkcube` na laptopie z renderowaniem na RTX 3090 Ti serwera.
2. Venus zakłada pamięć współdzieloną host↔gość (mapowanie "coherent"); przez sieć wymaga kopiowania przy map/unmap — może być bardzo wolne albo nie działać.
3. Obraz wraca do laptopa nieskompresowany (1366×768×4 B × 60 klatek ≈ 2 Gb/s) — za dużo dla kabla 1 Gb/s; potrzebny kompresowany powrót (NVENC → VA-API), czego Venus/vtest nie robi.

## Kroki
1. **PoC-1 (godziny):** serwer `virgl_test_server --venus` + `socat` TCP↔unix; laptop `socat` unix↔TCP + `VN_DEBUG=vtest vkcube` / `vulkaninfo`. Wynik: czy ICD Venus widzi GPU serwera (`deviceName` = RTX 3090 Ti) i czy `vkcube` rysuje.
2. **Pomiar:** warstwa tracingu `VK_LAYER` lub liczniki `socat`: bajty/s, liczba rund-trip na klatkę, rozkład rozmiarów; test z prostym demem i jedną grą (Proton/DXVK).
3. **Go/no-go:** jeśli ruch po kompresji < ~400 Mb/s i rund-trip/klatkę niewiele — projektujemy własny transport (agregacja poleceń, brak oczekiwania na odpowiedź, dyferencjowanie danych, kompresja wideo dla powrotu); w przeciwnym razie raport z liczbami i decyzja użytkownika (plan B = streaming).

## Wymaga roota (wykonuje użytkownik lub za jego zgodą)
- Laptop: `sudo pacman -S --needed vulkan-virtio vulkan-tools`
- Serwer: `sudo apt install -y virgl-server vulkan-tools socat`

## Zasady
Venus/virglrenderer: MIT. Nie kopiować kodu WiCi. Wynik PoC zapisać w AGENTS.md z liczbami.

## Cel funkcjonalny: odtworzyć to, co pokazało WiCi (transkrypcja filmu NetworkChuck)
Gra działa na laptopie, karta jest zdalna, obraz wraca skompresowany. Z filmu: fałszywy sterownik GPU na kliencie + cztery triki, dzięki którym z ~8 GB/s ruchu zrobiło się ~400 Mb/s i 92 FPS w Cyberpunku przez Wi-Fi (RTT 1,5 ms; u nas kabel, RTT 0,55 ms).

| Element WiCi | Nasz odpowiednik | Stan |
|---|---|---|
| fałszywy sterownik GPU | Venus (`vulkan-virtio`, Mesa, MIT) | istnieje |
| zdalne wykonanie | `virgl-server` + Vulkan NVIDIA | istnieje |
| trik 1: grupowanie/kompresja poleceń, bez czekania na odpowiedź | własny transport w Rust | do napisania |
| trik 2: tylko różnice w danych | dyferencjowanie buforów/tekstur | do napisania |
| trik 3: odpowiedzi z wyprzedzeniem | cache i przewidywanie odpowiedzi na zapytania | do napisania |
| trik 4: wynik jako skompresowane wideo | NVENC (serwer) + VA-API (laptop) + warstwa prezentacji | do napisania |

## Etapy (każdy kończy się liczbą)
1. PoC surowego Venus przez sieć; pomiar bazowy MB/s i rund-trip/klatkę (ile mają zaoszczędzić triki).
2. Pośrednik w Rust między gniazdem Venus (vtest) a serwerem: grupowanie + kompresja (trik 1).
3. Dyferencjowanie danych (trik 2).
4. Powrót obrazu jako wideo (trik 4) — wymaga, by ruch < 1 Gb/s.
5. Przewidywanie/cache odpowiedzi (trik 3), redukcja rund-trip.
6. Gra przez Proton/DXVK na laptopie; pomiar FPS i opóźnienia; cel: ≤ 400 Mb/s i stabilne 60 FPS.
Kryterium po etapach 1–2: czy ruch po kompresji i liczba rund-trip rokują; jeśli nie — raport i decyzja użytkownika (plan B = streaming).

## WYNIK PoC-1 (2026-10-31)
Przebieg: serwer `virgl_test_server --venus --no-virgl --rendernode /dev/dri/renderD128 --socket-path ...`; laptop `VK_DRIVER_FILES=virtio_icd.json VN_DEBUG=vtest VTEST_SOCKET_NAME=...`.
- **Na serwerze lokalnie: DZIAŁA.** `vulkaninfo` przez Venus widzi `Virtio-GPU Venus (NVIDIA GeForce RTX 3090 Ti)`, `driverName = venus`, exit 0. Stos Venus→NVIDIA jest sprawny.
- **Z laptopa przez tunel (ssh -L gniazdo unix) / socat: PADA.** Handshake przechodzi (serwer loguje "client context created"), potem klient kończy `abort()` wewnątrz libvulkan_virtio.so w `vkCreateInstance`. Zapis bajtów (socat -x) pokazuje: ostatnia wymiana to odpowiedź serwera na komendę 0x12 z 1 dodatkowym bajtem po danych.
- **Przyczyna (mocny wniosek z dowodu pośredniego, niezweryfikowany na poziomie kodu):** vtest przekazuje zasoby pamięci współdzielonej jako **deskryptory plików przez SCM_RIGHTS** (ten dodatkowy bajt to nośnik). Tunel SSH i socat nie przenoszą deskryptorów, więc klient dostaje błąd. Dowód: ten sam serwer i ten sam klient działają, gdy klient jest na tej samej maszynie.
- **Wniosek projektowy:** surowy Venus przez sieć jest niemożliwy. Potrzebny jest własny pośrednik, który **emuluje pamięć współdzieloną**: po stronie laptopa tworzy lokalne `memfd` i oddaje je klientowi Venus, po stronie serwera trzyma prawdziwy zasób, a zawartość synchronizuje przez sieć w punktach synchronizacji (przed komendami czytającymi pamięć gościa, przed odpowiedziami zależnymi od wyników hosta). To jest właśnie sedno WiCi (trik 2: tylko różnice; trik 1: grupowanie), nie dodatek. Etap 2 planu staje się więc obowiązkowy i jest głównym ryzykiem projektu.
- Uwaga środowiskowa: wymagało dodania użytkownika serwera do grup `render` i `video` (bez tego NVIDIA Vulkan zwracał ERROR_INITIALIZATION_FAILED).
- Narzędzia: `socat -x -v` jest dobrym podglądem protokołu; brakujących symboli Mesa (debuginfod Arch nie ma) nie da się odzyskać.
