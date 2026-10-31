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
