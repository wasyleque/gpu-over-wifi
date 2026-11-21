<!-- i18n-switch -->
[English](CALL-FOR-TESTERS.md) · **Polski** · [Español](CALL-FOR-TESTERS.es.md)
<!-- /i18n-switch -->

# Pomóż nam testować: zdalny GPU do gier na słabych maszynach

Szukamy osób z **różnymi laptopami, komputerami stacjonarnymi i sieciami**, które mogą uruchomić grę przez zdalny sterownik Vulkan i odesłać
wyniki. Obecnie wszystko zostało zmierzone na jednym kliencie (laptop Ryzen 5 4500U) i jednym serwerze (RTX 3090 Ti), więc nie wiemy jeszcze, jak stos zachowuje się
na innych procesorach, GPU, prędkościach łącza, systemach okienkowych czy grach. Każdy raport, w tym „od razu się wysypało”, jest przydatny.

## Co działa dzisiaj

* Natywne gry Linux Vulkan i gry Proton (DirectX 12 przez vkd3d-proton, DirectX 11 przez DXVK) na **kliencie Linux**, renderowane na serwerowym GPU NVIDIA,
  obraz zwracany jako H.264 (sprzętowe dekodowanie przez VA-API na kliencie) lub bezstratny LZ4.
* Dotychczas zmierzone: FurMark 93 FPS, American Truck Simulator 70 FPS (jasne sceny, 6 FPS w zatłoczonej scenie kokpitu), Hamster Hunter 21-34 FPS,
  Cyberpunk 2077 około 8 FPS (grywalne, 38 FPS w spokojnych scenach). Szczegóły: [README](../README.md#real-applications-ryzen-5-4500u-laptop---server-a-1-gbs-cable-h264-lab-runs).
* Jeszcze nie działa: klienci Windows, serwery inne niż NVIDIA, gry tylko OpenGL (Zink się wysypuje), wielu klientów na jednym GPU jednocześnie (jedna sesja na raz jest bezpieczna).

## Czego potrzebujesz

| | Serwer (ma mocny GPU) | Klient (słaba maszyna) |
|---|---|---|
| System | Linux | Linux (testowane Ubuntu 26.04 i Arch/Omarchy), Wayland lub X11 |
| GPU | NVIDIA z Vulkanem (testowany sterownik 580), użytkownik w grupach `render` i `video`; NVENC dla H.264 | dowolny GPU z dekodowaniem H.264 VA-API (Intel, AMD) lub użyj kodeka LZ4 |
| Sieć | **bezpośredni kabel 1 Gb/s** (lub lepszy) między dwiema maszynami; Wi-Fi nie jest zalecane (każda klatka czeka na ~50 round tripów) | tak samo |
| RAM | 8 GB+ | minimum 8 GB, **zalecane 16 GB** (sterownik przechowuje kopie zmapowanej pamięci gry) |
| Kompilacja | aktualny stabilny Rust (budujemy na 1.99) | aktualny stabilny Rust (budujemy na 1.99) |

Czas round-trip ma większe znaczenie niż przepustowość: kabel 0,15 ms dał 3x wyższą liczbę klatek na sekundę niż adapter USB 0,4 ms w naszych testach.

## Jak przeprowadzić test (30 minut)

1. Postępuj zgodnie z [`TESTING-OTHER-MACHINE.md`](TESTING-OTHER-MACHINE.md), aby zbudować i uruchomić demona serwera oraz sterownik klienta i uruchomić `vkcube`.
2. W przypadku gry ustaw opcje uruchamiania Steam (lub środowisko) w ten sposób na kliencie. `VK_LOADER_DRIVERS_SELECT` ma znaczenie: wartość odziedziczona ze Steam może ukryć sterownik.
   ```
   VK_DRIVER_FILES=$HOME/.local/share/gpu-hub/gpu_icd.json VK_LOADER_DRIVERS_SELECT=*gpu_icd* \
   GPU_ICD_ADDR=<server>:7979 GPU_ICD_TOKEN_FILE=$HOME/.config/gpu-hub/renderd.token \
   GPU_ICD_CODEC=h264 GPU_ICD_STATS=periodic GPU_ICD_LOG=$HOME/gpu-hub-icd.log %command%
   ```
   W przypadku gier Proton, które uruchamiają launcher, dodaj argument pomijający launcher gry (Cyberpunk: `--launcher-skip -skipStartScreen`).
3. Potwierdź, że gra naprawdę używa GPU serwera: własny log gry lub `vulkaninfo --summary` musi wskazywać GPU serwera, a `nvidia-smi` na serwerze musi pokazywać obciążenie.
4. Graj 5-10 minut w powtarzalnym miejscu (ta sama scena za każdym razem), a następnie zamknij grę normalnie.
5. Uruchom `tools/collect-report.sh <server-address> [path-to-icd-log]` i dołącz wynik do nowego zgłoszenia, używając szablonu **Raport z testu**.

## Co zawiera raport (nic tajnego)

Sprzęt i system klienta i serwera, czas round-trip i prędkość łącza, wersja sterownika oraz własne statystyki sterownika: FPS na okno 5 s, round
tripy na polecenie, kontencja blokad, wolumen przesłanej pamięci oraz raport watchdoga, jeśli wątek się zawiesił. Żadnych tokenów, żadnych ścieżek plików poza nazwą logu, żadnych zrzutów ekranu.
Podaj nam również grę, jej ustawienia graficzne i rozdzielczość oraz jak wyglądała płynność (sama liczba to nie wszystko).

## Jeśli coś pójdzie nie tak

* **Czarne okno lub „nie odpowiada” przez minutę na starcie:** poczekaj, ładowanie jest na początku powolne (shadery). Nie zabijaj gry przed upływem 2 minut.
* **Gra kończy działanie z komunikatem watchdoga lub awarią po wczytaniu zapisu:** dołącz własny raport o awarii gry i log sterownika (linie `WATCHDOG:` mówią, który wątek na co czekał).
* **Pamięć GPU serwera pozostaje zajęta po awarii:** zrestartuj demona (`pkill gpu-renderd`, uruchom go ponownie).
* **Nic się nie łączy:** sprawdź uprawnienia pliku tokenu i czy serwer nasłuchuje na adresie kabla, a nie na 0.0.0.0 (patrz [nota bezpieczeństwa](../README.md#security)).

## Licencja i bezpieczeństwo

Kod jest dostępny źródłowo na licencji FSL-1.1-ALv2 (bezpłatny do dowolnego niekonkurencyjnego użytku, Apache-2.0 po dwóch latach), patrz [`LICENSE`](../LICENSE). Demon
**nie jest zabezpieczony dla niezaufanych sieci**: używaj wyłącznie bezpośredniego kabla lub VPN i trzymaj token w tajemnicy.
