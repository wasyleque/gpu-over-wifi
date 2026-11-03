---
name: gpu-over-wifi-client
description: Konfiguracja i testowanie klienta projektu gpu-over-wifi (zdalny Vulkan: sterownik ICD na laptopie wysyła polecenia przez sieć do demona z kartą RTX na serwerze). Użyj, gdy użytkownik mówi o gpu-over-wifi, gpu-hub, zdalnej karcie, ICD, vkcube/vkmark/Dota przez zdalny GPU albo stawia nowy klient.
---

# gpu-over-wifi: klient (nowy laptop)

Repozytorium: https://github.com/wasyleque/gpu-over-wifi (Apache-2.0). **Najpierw przeczytaj `AGENTS.md` w repo**: to dziennik stanu, wnioski i pomiary. Szczegóły testów na innym sprzęcie: `docs/TESTING-OTHER-MACHINE.md`.

## Jak działa (w 5 zdaniach)
Na laptopie (klient) stoi nasz własny sterownik Vulkana `libgpu_icd.so` (crate `gpu-icd`). Aplikacja widzi zdalną RTX jako zwykłą kartę. Polecenia Vulkana idą przez TCP (z batchowaniem i kodekiem generowanym z `vk.xml`) do demona `gpu-renderd` na serwerze, który wykonuje je na prawdziwej karcie. Obraz wraca drugim połączeniem jako LZ4 albo H.264 i jest wyświetlany lokalnie (do tego potrzebny jest lokalny sterownik Vulkana: tu AMD `vulkan-radeon`, wcześniej Intel). Pamięć odwzorowana jest synchronizowana tylko różnicami (strony 4 KiB). Serwer: RTX 3090 Ti pod `10.42.0.2:7979`, demon uruchamiany z `~/gpu-hub/gpu-renderd`.

## Zasady współpracy z użytkownikiem
- Odpowiadaj po polsku, zwięźle; przed pomiarami z oknem na ekranie (vkcube, gra) **zapytaj**, bo użytkownik może oglądać film. Nie zabijaj procesów gry bez prośby.
- Oszczędzaj tokeny, ale nie kosztem jakości: logi skracaj (`tools/trace-summary.py`), budowanie i testy rób na serwerze (`tools/remote-build.sh`, `tools/check.sh`), małe zadania deleguj modelowi na `192.168.0.11` (Ollama, `mgraffam/qwen3.8-coder390:27b`, natywne API, `think:false`). Odpowiedzi modeli zawsze sprawdzaj.
- Token demona to **sekret**: nie wypisuj go, nie commituj, plik `chmod 600`. Nie bindować demona na interfejsy publiczne.

## Instalacja klienta na świeżym systemie (AMD Ryzen; Ubuntu 26.04 albo Arch/Omarchy)
1. Pakiety. **Ubuntu:** `sudo apt install build-essential git curl pkg-config libvulkan1 vulkan-tools mesa-vulkan-drivers mesa-va-drivers vainfo vkmark ffmpeg`, Rust przez `curl -sSf https://sh.rustup.rs | sh` (profil minimalny + `rustup component add clippy`), Steam: `steam-installer` (włącz multiverse i architekturę i386). **Arch/Omarchy:** `sudo pacman -S --needed base-devel git rust vulkan-icd-loader vulkan-tools vulkan-radeon libva-mesa-driver libva-utils vkmark ffmpeg steam`. Wymagane jądro 6.7+ (śledzenie zapisów pamięci; na starszym działa wolniej, bo skanuje całą pamięć: `GPU_ICD_NO_MEMTRACK=1` wymusza ten tryb).
2. `git clone https://github.com/wasyleque/gpu-over-wifi && cd gpu-over-wifi`.
3. `cargo build --release -p gpu-icd` (powstaje `target/release/libgpu_icd.so` i `gpu-smoke`).
4. Manifest: `mkdir -p ~/.local/share/gpu-hub && tools/icd-manifest.sh target/release/libgpu_icd.so ~/.local/share/gpu-hub/gpu_icd_release.json` (wersja formatu musi być `1.0.0`; kontener Steama odrzuca `1.0`).
5. Token: poproś użytkownika o skopiowanie pliku tokenu klienta ze starego laptopa (`scp stary:~/.config/gpu-hub/renderd-2.token ~/.config/gpu-hub/`), potem `chmod 600`. Nie generuj go sam.
6. Sieć (stan aktualny): serwer `gpu-hub` ma **dwa adresy**: kabel `10.42.0.2/24` (karta wbudowana, wpięta do mostu medialnego ASUS) oraz Wi-Fi USB `192.168.1.83/24` (stały adres z DHCP routera RT-AC68U) do zarządzania (`ssh wasyl@192.168.1.83`). Most nie przekazuje ruchu serwera do routera, ale urządzenia wpięte kablem do mostu widzą się nawzajem: **ten laptop dodaje na swojej karcie kablowej adres `10.42.0.3/24`** (`sudo ip addr add 10.42.0.3/24 dev <karta_kablowa>`) i łączy się z demonem po `10.42.0.2:7979`. Demon słucha TYLKO na kablu; nie otwieraj go na Wi-Fi/LAN bez zgody użytkownika (protokół nie jest szyfrowany, jedyną ochroną jest token).
7. Zmienne środowiska do testów: `GPU_ICD_ADDR=10.42.0.2:7979 GPU_ICD_TOKEN_FILE=$HOME/.config/gpu-hub/renderd-2.token VK_DRIVER_FILES=$HOME/.local/share/gpu-hub/gpu_icd_release.json`.

## Testy po kolei
`vulkaninfo --summary` (powinna być RTX 3090 Ti, Vulkan 1.3) → `target/release/gpu-smoke` (+ `gpu-smoke swapchain`) → `vkcube --wsi wayland` → `vkmark -D <UUID RTX z vkmark -L> --present-mode immediate` (mailbox celowo nieogłaszany) → gra. Oczekiwane: vkcube kilkaset FPS, vkmark 100–200 FPS w 720p.

## Gra ze Steama (Linux Runtime / pressure-vessel) — opcje uruchamiania
```
VK_DRIVER_FILES=$HOME/.local/share/gpu-hub/gpu_icd_release.json:/usr/share/vulkan/icd.d/radeon_icd.json VK_LOADER_DRIVERS_DISABLE=*radeon* GPU_ICD_ADDR=10.42.0.2:7979 GPU_ICD_TOKEN_FILE=$HOME/.config/gpu-hub/renderd-2.token GPU_ICD_CODEC=lz4 GPU_ICD_STATS=1 GPU_ICD_DEBUG=1 GPU_ICD_LOG=/tmp/gpu-icd.log GPU_ICD_TRACE=/tmp/gpu-icd-trace.txt %command% -vulkan -windowed
```
Zmienne muszą stać **przed** `%command%`. Lokalny sterownik (Mesa) musi być na liście `VK_DRIVER_FILES`, bo kontener ukrywa sterowniki hosta, a nasz ICD potrzebuje go do okien; `VK_LOADER_DRIVERS_DISABLE` ukrywa go przed samą grą, żeby wybrała zdalną kartę. Test bez gry w tym samym kontenerze: `~/.local/share/Steam/steamapps/common/SteamLinuxRuntime_4/_v2-entry-point --verb=run -- vulkaninfo --summary`.

## Diagnostyka
`GPU_ICD_LOG` (stderr ICD do pliku), `GPU_ICD_TRACE` (polecenia z czasami; podsumuj `python3 tools/trace-summary.py plik`), `GPU_ICD_PROCLOG` (funkcje, o które pytano, a których nie ma), `GPU_ICD_DUMP_FRAME=plik.ppm GPU_ICD_DUMP_AT=N` (zapis klatki N), na serwerze `GPU_RENDERD_TRACE`. Awarie gier: `coredumpctl info`, a `gdb` na zrzucie pokazuje wywołanie pustego wskaźnika (zwykle brakująca funkcja Vulkana).

## Stan wydajności (przed testami na mocniejszym laptopie)
American Truck Simulator (`-rdevice vk`, natywny Linux) renderuje poprawnie na zdalnej RTX; na starym i5-6300U ok. 31 FPS w 1366x768, ale obraz ma kilkuklatkowe opóźnienie, a po długiej grze gra się zawiesza. Największe koszty (liczniki co 5 s w `GPU_ICD_LOG` przy `GPU_ICD_DEBUG=1`): czekanie na fence (`vkWaitForFences` ok. 46% czasu, 5 obiegów po sieci na klatkę), odświeżanie odwzorowanej pamięci po oczekiwaniu (ok. 2 ms na odświeżenie), skan stron. Plan: asynchroniczne zdarzenia "fence gotowy" z serwera, rzadsze odświeżanie pamięci, cache wymagań pamięci; potem nowe kodeki (HEVC). Manifest Radeona to `radeon_icd.json` (Ubuntu 26.04 i Arch; starsze wydania mają `radeon_icd.x86_64.json`): sprawdź `ls /usr/share/vulkan/icd.d`.

## Znane pułapki i stan (szczegóły w AGENTS.md)
- Dota 2 (natywny Vulkan) dociera do menu, ale klatki są czarne, a obiegów po sieci jest ok. 117 na klatkę (10–20 FPS). To są otwarte tematy: zrzut klatki Doty oraz redukcja zapytań synchronicznych (cache wymagań pamięci, vkGetFenceStatus, spekulacja).
- Operacje, które mogą się legalnie nie powieść, muszą być synchroniczne (alokacja pamięci i zestawów deskryptorów, tworzenie potoków z `FAIL_ON_PIPELINE_COMPILE_REQUIRED`); null-uchwytów serwer nie rejestruje.
- `VK_EXT_graphics_pipeline_library` jest wyłączone (sterownik NVIDII na serwerze pada). Duże pamięci write-combined nie są odświeżane z serwera (odczyt jest tam bardzo wolny).
- Karta AMD dekoduje sprzętowo także **HEVC** (i965 tylko H.264): `Codec::Hevc` jest zarezerwowany w protokole, brak implementacji (NVENC HEVC działa na serwerze, brak dekodera VA-API HEVC po stronie klienta).
- Jeśli demon na serwerze nie żyje: `ssh wasyl@10.42.0.2`, katalog `~/gpu-hub`, uruchom `./gpu-renderd --token-file renderd-token --listen 10.42.0.2:7979` (`GPU_RENDERD_TRACE=/tmp/renderd-trace.txt` dla diagnostyki). Zwolnienie VRAM po DeepSeeku: zabić `inferssd-server`.
