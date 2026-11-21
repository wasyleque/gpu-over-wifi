# Analiza filmu WiCi One (2026-11-21)

Transkrypcja filmu przeanalizowana przez DeepSeek (deepseek-v4-pro). Wniosek: film ujawnia tylko poziom ogólny — własny sterownik-ICD, polecenia grupowane i kompresowane bez czekania na odpowiedź, wysyłanie samych zmian danych, "odpowiedzi bez czekania na pytanie" (przewidywanie), skompresowane wideo z powrotem do klienta. To WSZYSTKO już mamy (batch, delta, wczesne odpowiedzi, okna gorące, NVENC/VA-API). Reszta filmu to LLM z SSD i scheduler modeli (nie dotyczy Vulkana). Liczby z filmu (92 FPS Blender, 400 Mb/s) bez metodyki — nie do porównania. Nic nowego do wdrożenia poza kierunkiem: dalej zmniejszać liczbę zależnych punktów synchronizacji na klatkę.

## Pełna odpowiedź modelu

## 1. Ujawnione techniki (z cytatami/znacznikami)

- **Zdalny sterownik / „fałszywy GPU driver”**  
  [4:17–4:27] „They did that by installing a fake driver.”  
  Klient dostaje sterownik, który udaje lokalną kartę graficzną. To jest warstwa typu ICD/remoting.

- **Własny protokół i redukcja „gadatliwości” API**  
  [4:39–5:02] „They figured out which messages actually need answers and then stopped waiting on the rest. For instructions, they bundled them, compressed them, and sent them without waiting. So for data, it only sends what’s changed and doesn’t keep repeating itself. For answers, it doesn’t even wait for the question. It knows what’s going to be asked.”  
  Techniki: batchowanie komend, kompresja, async bez czekania, delta danych, prefetch/odpowiedzi z wyprzedzeniem.

- **Kompresja wideo zwracana z GPU do klienta**  
  [5:04–5:07] „And then the GPU sends compressed video back to the laptop.”  
  Finalny obraz jest przesyłany jako skompresowane wideo, co redukuje wymagane pasmo.

- **Optymalizacja odczytu modelu AI z SSD: wyrównane chunki, odczyt 64 chunków naraz**  
  [14:00–14:33] „rewrote the model files … every expert is the same size. Aligned chunks … reading it 64 chunks at a time … from one gigabyte per second to around 11.5 gigabytes per second.”

- **Cache popularnych ekspertów MoE w VRAM/RAM**  
  [14:40–14:52] „keeps the most popular ones sitting on VRAM and RAM … only around one gigabyte worth of experts is being pulled from the SSD.”

- **Split obliczeń CPU/GPU w zależności od miejsca trzymania wag**  
  [15:02–15:08] „If the weights are coming off the SSD or RAM, the CPU is going to handle it. But if the weights are coming off VRAM, the GPU is going to handle it.”

- **Scheduler „parkowania” modeli na SSD przy dzieleniu GPU**  
  [19:07–19:31] „The scheduler … will move that model down to the SSD … When that app needs to generate another image, it can quickly bring it out of SSD.”  
  Demo: przełączenie bez parkowania ~17–18 s, z schedulerem ~4 s [20:48–21:00].

---

## 2. Fakty vs marketing

**Prawdopodobne fakty techniczne / realne mechanizmy:**
- Zdalny sterownik/ICD jest realny i potrzebny.
- Batchowanie, kompresja, delta, async i prefetch to realne techniki zmniejszające liczbę round-tripów.
- Kompresja wideo do prezentacji finalnej klatki to realna technika.
- Wyrównywanie chunków i cache ekspertów na SSD to realne techniki przyspieszające LLM na SSD.

**Marketing / nieweryfikowalne:**
- Film jest sponsorowany; to wczesny egzemplarz inżynieryjny.
- „92 FPS w Blenderze” [4:02] — brak sceny, rozdzielczości, trybu widoku, jakości.
- „400 Mb/s przy graniu” [5:16–5:19] — pomiar bez kontroli warunków Wi-Fi, pakietów, retransmisji.
- „750B model na 32 GB VRAM działa” — tak, ale tylko z SSD i przy zmiennym 3–15 token/s.
- Scheduler „4 s vs 17 s” — demo, nie kontrolowany benchmark.

---

## 3. Nasza architektura: czy wdrożyć, zysk, pomiar

Nasz przypadek: zdalny Vulkan ICD + daemon z RTX 3090 Ti, LAN 2,5 GbE, Cyberpunk przez vkd3d-proton = **25,5 FPS, ~28 zależnych punktów synchronizacji, GPU busy 10–15%**.  
Główne ograniczenie to CPU klienta i wiele wait/round-tripów, nie samo pasmo sieci.

| Technika z filmu | Czy wdrożyć u nas? | Spodziewany zysk w FPS | Jak zmierzyć |
|---|---:|---:|---|
| **Zdalny sterownik/ICD** | Już mamy własny ICD | 0 bezpośrednio; zysk = kompatybilność, poprawność | Testy API/ICD, aplikacja widzi RTX 3090 Ti |
| **Batchowanie/kompresja komend, wysyłanie bez czekania** | Tak, priorytetowo | **Wysoki**; potencjalnie redukcja 28 sync-pointów do kilku, wzrost nawet do 40–60 FPS, jeśli średni stall to ~1 ms | Liczba round-tripów na klatkę, czas `vkQueueSubmit`/`vkWaitForFences`, CPU klienta, GPU busy, PresentMon/FrameView |
| **Delta danych: wysyłanie tylko zmienionych zakresów** | Tak, w daemonie/ICD | Umiarkowany; mniej transferu i CPU klienta, może dać 5–15% | Bajty wysłane na klatkę, czas map/unmap, FPS |
| **Odpowiedzi wyprzedzające / prefetch** | Częściowo; w Vulkanie trudno „zgadywać odpowiedź”, ale można robić async readback, cache stanów, prefetch zasobów | Wysoki, bo usuwa zależne waity CPU↔GPU | Rozkład czasów `vkGetFenceStatus`, `vkGetQueryPoolResults`, liczba stallów |
| **Kompresja wideo z GPU do klienta** | Tak, jeśli wysyłamy finalny obraz; NVENC na serwerze, klient dekoduje | 0–15% FPS, głównie jeśli słaby CPU klienta lub duże klatki; może poprawić płynność, ale dodaje latencję | End-to-end latency, frame time, zużycie CPU klienta, pasmo sieci |
| **Optymalizacja SSD pod LLM / cache ekspertów / split CPU-GPU** | Nie dotyczy zdalnego Vulkan/Cyberpunka | 0 FPS w Cyberpunku; ewentualnie dla przyszłego AI | Token/s, czas ładowania modelu, przepustowość SSD |
| **Scheduler parkowania modeli na SSD** | Opcjonalnie, jeśli będziemy współdzielić GPU między wieloma aplikacjami/klientami | 0 FPS w pojedynczym Cyberpunku; korzyść w przełączaniu zadań | Czas przełączania kontekstu/modelu, GPU busy, VRAM usage |

**Pomiar ogólny:**  
Używaj A/B na tej samej scenie, np. Cyberpunk benchmark/stacjonarny. Zbieraj: FPS, frame time percentyle, GPU busy %, CPU busy %, liczbę i czas wait/fence, liczbę round-tripów, pasmo. Loguj w ICD/daemonie każdy `vkQueueSubmit`, `vkWaitForFences`, `vkGetQueryPoolResults`, `vkMapMemory`.

---

## 4. Czego film NIE mówi

- **Nie wiadomo, czy to naprawdę remoting API, czy hybryda z wideo.** Film mówi jednocześnie o „fake driver”, „gra chodzi na laptopie”, ale też „GPU sends compressed video back”. To sugeruje remoting API dla części komend + strumień wideo dla prezentacji, ale brak szczegółów.
- **Brak informacji o warstwie przechwytującej:** Vulkan, DXGI, Metal? Gdzie kompilacja shaderów, alokacja pamięci, swapchain?
- **Brak specyfikacji testów:** rozdzielczość/ustawienia Cyberpunka, model laptopa, CPU, iGPU, Wi-Fi 6/6E/7, odległość, zakłócenia, packet loss.
- **Brak danych o latencji:** end-to-end latency, input lag, jakość kompresji wideo.
- **Brak szczegółów protokołu:** retransmisje, FEC, kolejkowanie, bezpieczeństwo, multi-klient.
- **Nie pokazano GPU busy/CPU clienta** w Cyberpunku; tytułowe „Wi-Fi GPU” pokazuje FPS tylko dla Blendera.
- **AI na SSD:** nie podano, czy model jest kwantyzowany, jaki dokładnie SSD, jaki CPU, ile RAM, jaka długość kontekstu, jak wygląda wydajność przy dłuższej sesji.
- **Scheduler:** nie pokazano, jak zachowuje się przy więcej niż dwóch aplikacjach, jaki jest narzut kopiowania na/z SSD, zużycie SSD.