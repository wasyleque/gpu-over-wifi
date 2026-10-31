# E5d — Badanie: zdalne wywołania Vulkan ("GPU jak zamontowany") — spike 1–2 dni

Cel: odpowiedzieć liczbami, czy mechanizm B (patrz `docs/DESIGN-game-streaming.md` §1) jest realny dla tego sprzętu.

## Pomiar
1. Warstwa `VK_LAYER` (tylko tracing, bez sieci) logująca każde wywołanie Vulkan: nazwa, rozmiar argumentów, rozmiar przesyłanych buforów, czy wywołanie wymaga odpowiedzi (rund-trip).
2. Uruchomić `vkcube`, potem proste demo i jedną realną grę (Proton/DXVK) przez minutę.
3. Raport: wywołań/s, MB/s poleceń, MB/s danych (tekstury/bufory), liczba rund-trip/klatkę, rozkład rozmiarów.

## Kryterium go/no-go
- Szacunek przepustowości po prostej kompresji i dyferencjowaniu danych < 400 Mb/s (poziom, który WiCi deklaruje) **oraz** rund-trip/klatkę < ~5 po grupowaniu → kontynuować prototyp serializacji.
- W przeciwnym razie: zostajemy przy strumieniowaniu (A) i zdalnym CUDA (E7) tylko dla obliczeń.

## Uwagi
Nie kopiować kodu WiCi ani cudzych implementacji bez zachowania licencji; sprawdzić licencje projektów (Venus/virglrenderer: MIT) przed użyciem.
