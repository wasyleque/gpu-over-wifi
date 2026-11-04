# Test na innym zestawie (Linux; Windows jeszcze nie)

Stan na 2026-11-02. **Linux jako klient działa** (zmierzone na laptopie z Intel HD 520 + zdalna RTX 3090 Ti). **Windows nie jest jeszcze obsługiwany**: wymaga osobnego ICD dla Windows (etap M8: inna nazwa biblioteki i manifest w rejestrze, WSI Win32, DXGI). Serwer (`gpu-renderd`) działa na Linuksie z kartą NVIDIA, AMD lub Intel ze sterownikiem Vulkan.

## 1. Serwer (maszyna z kartą, Linux)
```
cargo build --release -p gpu-renderd          # albo cargo zigbuild --target x86_64-unknown-linux-gnu.2.39 dla starszego glibc
head -c 24 /dev/urandom | base64 > renderd-token && chmod 600 renderd-token
./target/release/gpu-renderd --token-file renderd-token --listen <ADRES-KABLA>:7979
```
Domyślnie nasłuchuje tylko na 127.0.0.1; adres sieciowy podajesz jawnie (najlepiej tylko interfejs kabla, nie 0.0.0.0). Użytkownik serwera musi należeć do grup `render` i `video`. W logu demona przy każdej instancji widać, jakie urządzenia widzi (`widzi N urządzeń: [...]`); jeśli widać tylko `llvmpipe`, sterownik GPU nie działa dla tego procesu (restart demona pomaga).

## 2. Klient (maszyna, na której ma działać gra, Linux)
```
cargo build --release -p gpu-icd -p gpu-renderd
tools/icd-manifest.sh target/release/libgpu_icd.so ~/.local/share/gpu-hub/gpu_icd.json
scp serwer:renderd-token ~/.config/gpu-hub/renderd.token && chmod 600 ~/.config/gpu-hub/renderd.token
export GPU_ICD_ADDR=<ADRES-SERWERA>:7979 GPU_ICD_TOKEN_FILE=~/.config/gpu-hub/renderd.token
export VK_DRIVER_FILES=~/.local/share/gpu-hub/gpu_icd.json      # tylko nasz ICD jest widoczny dla aplikacji
```
Lokalna karta do WYŚWIETLANIA jest wykrywana automatycznie (kolejność: Intel, AMD, NVIDIA, inne; pomijane są lvp/virtio/gfxstream). Nadpisanie: `GPU_ICD_LOCAL_ICD=/usr/share/vulkan/icd.d/<manifest>.json`.

## 3. Testy po kolei (od najprostszego)
1. `target/release/gpu-probe <ADRES-SERWERA>:7979 --token-file <token>`: lista kart serwera i czas zapytania.
2. `tools/icd-smoke.sh <ADRES>:7979 <token> "<fragment nazwy karty>"`: `vulkaninfo --summary` widzi zdalną kartę.
3. `target/release/gpu-smoke 20`: bufor i obraz na zdalnej karcie, odczyt. `gpu-smoke surface` i `gpu-smoke swapchain`: powierzchnia i swapchain na powierzchni headless (bez okna).
4. `GPU_ICD_STATS=1 vkcube --wsi wayland --c 600 --present_mode 0` (albo `--wsi xcb`): kostka na zdalnej karcie. Zrzut pierwszej klatki: `GPU_ICD_DUMP_FRAME=/tmp/klatka.ppm`.

## 4. Opcje
- `GPU_ICD_CODEC=auto|raw|lz4` (dziś dostępne raw i lz4; `h264` i `hevc` są w protokole i czekają na NVENC oraz dekoder; wybór niedostępnego kodeka daje ostrzeżenie i lz4).
- `GPU_ICD_SYNC_PRESENT=1`: tryb synchroniczny (bez osobnego połączenia wideo), do diagnozy.
- `GPU_ICD_ALL_DEVICES=1`: pokaż też urządzenia CPU serwera (llvmpipe).
- `GPU_ICD_STATS=1`: statystyki na stderr przy zamknięciu; `GPU_ICD_DEBUG=1`, `GPU_ICD_DEBUG_LOCAL=1`: diagnostyka.

## 5. Co mi odesłać po teście
- Linie `gpu-icd statystyki: ...` i `gpu-icd wideo: ...` (bajty na klatkę, czasy etapów, obiegi po sieci).
- Model lokalnej karty (`vulkaninfo --summary`), wersja Mesa/sterownika, kompozytor (Wayland/X11), łącze (kabel/Wi-Fi) i jego RTT (`ping`).
- Wynik `vainfo` (do wyboru kodeka H.264/HEVC po stronie klienta).
- Czy okno pokazuje kostkę poprawnie; jeśli nie, komunikat błędu i `GPU_ICD_DEBUG=1`.

## 6. Znane ograniczenia
- Obraz wraca jako H.264 na osobnym połączeniu (dekodowanie VA-API na kliencie); ograniczeniem są obiegi po sieci i praca klienta, nie serwer (stan i plan: `docs/consult/11-next-steps.md`).
- Gry działają (FurMark, ATS, Hamster Hunter, Cyberpunk 2077 przez Proton; wyniki w README), ale tylko na jednym kliencie; testy na innych maszynach: [`CALL-FOR-TESTERS.md`](CALL-FOR-TESTERS.md). Wiele rozszerzeń Vulkana nie jest ogłaszanych (zasada: tylko to, co działa).
- Demon na jednym serwerze bywał niestabilny w wyliczaniu karty NVIDIA po dłuższym czasie (przyczyna niewyjaśniona; patrz AGENTS.md).
- Uwierzytelniony klient może wysłać niepoprawne polecenia Vulkana i zawiesić sterownik serwera: używaj tokena i zaufanej sieci.
