# E5a — Provisioning serwera `gpu-hub` pod gry (wymaga roota; uruchamia użytkownik)

Cel: nowy serwer (Ubuntu 26.04, RTX 3090 Ti, sterownik 580-server) potrafi uruchomić pulpit z grami bez fizycznego monitora i kodować go przez NVENC. Skrypt `~/provision-games.sh` pisze nadzorujący, użytkownik uruchamia `sudo bash`.

## Zakres skryptu
1. `dpkg --add-architecture i386`; `apt update`.
2. Pakiety: `xserver-xorg-core xserver-xorg-video-nvidia-580-server` (sprawdzić nazwę przez `apt-cache policy`), `openbox`, `xterm`, `x11-xserver-utils` (xrandr), `pipewire pipewire-pulse wireplumber`, `steam-installer` (multiverse), `libvulkan1:i386 mesa-vulkan-drivers:i386`, `gamemode`.
3. **Sunshine**: pobrać `sunshine_2026.914.233613-1+ubuntu26.04_amd64.deb` z wydania GitHub LizardByte/Sunshine, **zweryfikować SHA-256 względem sumy z API wydania**, instalować przez `apt install ./plik.deb`. Przerwać, jeśli suma się nie zgadza.
4. Konfiguracja Xorg bez monitora: `/etc/X11/xorg.conf.d/20-gpuhub-headless.conf` (`AllowEmptyInitialConfiguration`, `ConnectedMonitor`, wirtualny ekran do 3840×2160).
5. Usługi: `gpuhub-xorg.service` (system, Xorg :0), `gpuhub-desktop.service` (użytkownik: openbox), Sunshine jako usługa użytkownika; wszystko **wyłączone z autostartu** (hub włącza na czas sesji). Użytkownik w grupach `video`, `render`, `input`; `loginctl enable-linger`.
6. Wirtualne wyjście dźwięku (PipeWire null sink) dla Sunshine.
7. Hook `prep-cmd` Sunshine: skrypt ustawiający tryb X na rozdzielczość klienta (`cvt` + `xrandr --newmode/--addmode`), zmienne `SUNSHINE_CLIENT_WIDTH/HEIGHT/FPS`.
8. Reguły `ufw` tylko dla `10.42.0.0/24`: TCP 47984, 47989, 47990, 48010; UDP 47998, 47999, 48000, 48002.

## Weryfikacja
- `systemctl status` usług; `DISPLAY=:0 xrandr` pokazuje wirtualny monitor; `nvidia-smi` pokazuje proces Xorg.
- Sunshine odpowiada na `https://10.42.0.2:47990` z laptopa; `moonlight pair 10.42.0.2` przechodzi z PIN-em.
- Pulpit w Moonlighcie 1366×768@60; nakładka statystyk (Ctrl+Alt+Shift+S) pokazuje opóźnienie sieci < 2 ms i czas dekodowania.

## Zasady
Brak `curl | sh`. Każdy pobierany plik weryfikowany sumą. Skrypt idempotentny (można uruchomić ponownie). Nie dotyka dysków ani `/srv/models`.
