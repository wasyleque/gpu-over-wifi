# E5c — hubctl game (klient Linux/Windows)

Zależy od E5b.

## Wymagania
- `hubctl game start [--app Desktop|Steam] [--res auto|WxH] [--fps 60] [--codec h264|hevc] [--no-launch]`:
  1. `POST /v1/game/session`; czeka na gotowość (timeout 120 s);
  2. uruchamia `moonlight stream <host> <app> --resolution WxH --fps N --video-codec H.264` (`auto` = rozdzielczość aktywnego monitora: Hyprland `hyprctl monitors -j`, inaczej 1920×1080);
  3. po wyjściu z Moonlighta: `DELETE /v1/game/session` (chyba że `--keep`).
- `hubctl game status`, `hubctl game stop`.
- `hubctl game pair`: wywołuje `moonlight pair <host>` i wypisuje, gdzie wpisać PIN (interfejs Sunshine).
- Brak `moonlight` w PATH → czytelny błąd z poleceniem instalacji (Arch: `pacman -S moonlight-qt`; Ubuntu: flatpak).
- Windows: ta sama składnia; ścieżka `Moonlight.exe` z rejestru lub `--moonlight-path`.

## Hyprland (osobny, mały krok)
Skrót w `~/.config/hypr/bindings.lua` uruchamiający `hubctl game start` — dodawać zgodnie ze skillem `omarchy` (sprawdzić istniejące wiązania, `hyprctl reload`, `hyprctl configerrors`).

## Testy
- Budowa argumentów Moonlighta dla różnych opcji (czysta funkcja).
- Wybór `auto` z przykładowego JSON `hyprctl monitors -j`.
- Serializacja nowych typów `hub-proto`.
