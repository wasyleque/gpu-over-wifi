# Burza mózgów: dalsze kroki optymalizacji (stan 2026-11-05)

## Gdzie jesteśmy

| Gra | FPS | Co zjada klatkę |
|---|---|---|
| ATS (lekka scena) | 68-72 | prawie nic: 11 tys. obiegów na sesję |
| ATS (kokpit, lustra) | 6 | 16,6 MB/klatkę zapisów (7,4 MB po LZ4) do jednego ringu 256 MB, czekanie za bajtami w strumieniu |
| Cyberpunk 2077 | 7,6 (do 38 w spokojnych scenach), "grywalny" | 76 obiegów/klatkę (43x `vkWaitSemaphores`), kolejka do blokady (210 ms/klatkę sumarycznie), 5,4 MB zapisów/klatkę, skan 23 ms/klatkę |

Serwer wykonuje całą klatkę w ok. 12 ms, GPU jest prawie bezczynne, łącze jest w średniej wolne (160 Mb/s w CP). Granica przepustowości 1 Gb/s to 118 MB/s,
czyli przy 2,3 MB/klatkę teoretycznie ok. 50 FPS; do 30 FPS potrzeba obciąć łańcuch oczekiwań i pracę klienta.

## Kierunki, od największego efektu do najmniejszego

**1. Oczekiwania na GPU bez obiegu (największy zysk w CP: 27+3 ms/klatkę).**
Serwer wypycha wartości zakończenia semaforów czasowych i fence'ów (wątek obserwujący + kanał na połączeniu wideo), klient odpowiada lokalnie
na `vkGetSemaphoreCounterValue`, a `vkWaitSemaphores` wraca bez sieci, gdy wartość już jest. Zostaje obieg tylko dla naprawdę przyszłych wartości.
Ryzyko: spójność z odświeżaniem pamięci (rozwiązane przez odczyt leniwy), zawsze weryfikować audytem.

**2. Odcinkowe oczekiwania (już w kodzie, `GPU_ICD_SLICE_WAIT=1`, do testu w grze).** Długie oczekiwanie nie blokuje strumienia serwera ani blokady
klienta. Docelowo osobne połączenie "oczekiwań" z własnym wątkiem serwera (żeby oczekiwania nie wiązały się z odpytywaniem).

**3. Mniej wejść w blokadę instancji (210 ms/klatkę oczekiwania sumarycznie).** `vkUpdateDescriptorSets` (ok. 1 200/klatkę w ATS) i spekulatywne
alokacje zestawów (ok. 900/klatkę) przechodzą przez globalną blokadę. Rozwiązanie: partia poleceń w osobnym mutexie krótkiego trzymania (`batch` poza
`InstState`), `next_id` jako atomic, księgowość pul w osobnym mutexie. Drugie podejście: per-zestaw bufor zmian drenowany przy pierwszym `vkCmdBindDescriptorSets`.

**4. Objętość zapisów pamięci (ATS 16 MB/klatkę, CP 5 MB/klatkę).**
- Różnice względem poprzedniej zawartości strony (XOR + LZ4 po stronie klienta, serwer ma poprzednią treść): potencjalnie największe dla pierścieni
  przepisywanych częściowo. Wymaga drugiej kopii pamięci po stronie klienta (stąd sens 16 GB RAM).
- Dedupe treści na serwerze (ankieta: 32 % stron powtórzonych) i lepszy kompresor niż LZ4 (zstd poziom 1) dla treści kompresowalnej.
- Kompresja na puli wątków klienta zamiast w wątku wołającym pod blokadą; zapis przez wiele połączeń TCP.
- Śledzenie, które zakresy GPU rzeczywiście czyta (dziś wszystko brudne jest wysyłane).

**5. Skan pamięci (23 ms/klatkę w CP).** Dokładne śledzenie jądrem (`PAGEMAP_SCAN` jednym wywołaniem na arenę zamiast haszowania małych
pamięci i wywołania na każdą pamięć), wspólna arena dla wszystkich odwzorowanych pamięci klienta.

**6. Głębsze potokowanie klatek.** `GPU_ICD_MAX_INFLIGHT` 3 -> 4-6, gdy oczekiwania są już krótkie; sprawdzić wpływ na opóźnienie sterowania.

**7. Jakość i niezawodność.** Sprzątanie sesji serwera po twardym zerwaniu (15 GB VRAM zostało po awariach gry), limity rejestru uchwytów, test na
wielu wątkach klienta (model: kilka wątków vkd3d), pomiar w tej samej scenie (powtarzalna trasa), kolejne gry (Unity, DXVK), Zink/Heaven.

## Skalowanie obrazu i generowanie klatek (pytanie użytkownika)

Obecne wąskie gardło to koszt klatki po stronie klienta (obiegi, blokada, zapisy), nie piksele: GPU serwera jest bezczynne. DLSS/FSR/XeSS (skalowanie)
nie zmniejszają liczby poleceń ani zapisów, więc nie przyspieszą gry, a obniżyłyby jakość. Generowanie klatek (FSR 3 FG, w CP 2.x; DLSS-G odpada na RTX 3090 Ti)
zwiększałoby wyświetlany FPS bez dodatkowych poleceń, ale ma sens dopiero od ok. 30+ FPS bazowych. Skalowanie po stronie klienta (render niżej, powiększenie
na laptopie) oszczędza tylko łącze i dekoder (4-5 ms + 3-4 ms na klatkę, osobny wątek), kosztem ostrości. Odłożone, wracamy po osiągnięciu >= 30 FPS bazowych.

## Realistyczna prognoza

Kroki 1-3 zdejmują z klatki CP ok. 60-80 ms z 130-140 ms, czyli 12-16 FPS bazowych. Kroki 4-5 dają kolejne 20-30 ms w scenach z dużymi zapisami (ATS kokpit,
CP w mieście). 30 FPS wymaga wszystkich sześciu i nadal zależy od procesora Ryzena 4500U, na którym działa logika gry i generowanie poleceń. Pomiar 100 %-wego
obciążenia wątków gry na laptopie jest warunkiem, zanim obiecamy liczbę.
