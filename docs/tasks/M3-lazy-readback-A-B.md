# M3 odczyt leniwy: zadania A i B (dla agy)

Kontekst: `crates/gpu-icd` (sterownik Vulkan po stronie klienta) po każdym oczekiwaniu na GPU odświeża z serwera pamięć
odwzorowaną przez aplikację (`fwd.rs`, `read_mapped_inner`). Dla gier z wieloma dużymi buforami zapisywalnymi przez GPU to
dziesiątki MB na każde oczekiwanie, choć aplikacja zwykle tych danych nie czyta. Cel całości: pobierać zakres z serwera
dopiero wtedy, gdy aplikacja go dotknie (błąd strony). Poniżej dwa **niezależne, małe moduły**, które mają powstać
jako NOWE pliki. Integracją (blokady, `userfaultfd`, `fwd.rs`) zajmuje się Claude: nie ruszaj innych plików.

## Zasady (obowiązują oba zadania)

- Katalog: `/home/wasyl/Projects/gpu-hub`. Pracuj tylko w nowych plikach wskazanych niżej oraz dopisz **jedną linię** `mod <nazwa>;`
  w `crates/gpu-icd/src/lib.rs` obok istniejących `mod ...;` (około linii 25-35). Żadnych innych zmian w istniejących plikach.
- Nie dodawaj zależności (brak crate'a `libc`): wywołania systemowe deklaruj jak w `crates/gpu-icd/src/memtrack.rs`
  (`extern "C" { fn mmap(...); ... }`). Przeczytaj ten plik, żeby zachować styl.
- Komentarze, nazwy testów i komunikaty po polsku/angielsku tak jak w sąsiednim kodzie (komentarze po polsku, nazwy w kodzie po angielsku).
- Każda funkcja `unsafe` ma sekcję `# Safety` z opisem wymagań; każdy blok `unsafe` ma komentarz `// SAFETY:`.
- Nie używaj `unwrap()`/`expect()` poza testami. Bez `panic!` w kodzie produkcyjnym.
- Weryfikacja: `cd /home/wasyl/Projects/gpu-hub && cargo test -p gpu-icd <nazwa_modułu> && cargo clippy -p gpu-icd --all-targets`.
  Clippy musi być czysty (zero ostrzeżeń). Nie uruchamiaj `tools/check.sh` ani budowania całego workspace.
- Nie wykonuj żadnych poleceń `git` (commity robi Claude). Nie ruszaj procesów ani plików poza repo.
- Na końcu odpowiedz krótko po polsku: co powstało, wynik testów i clippy, ewentualne wątpliwości.

## Zadanie A: `crates/gpu-icd/src/lazy.rs` (czysta logika, bez `unsafe`)

Stan nieaktualności stron lokalnej kopii jednej pamięci. Strona = 4096 bajtów.

```rust
pub const PAGE: usize = 4096;

pub struct LazyPages { /* bitset nieaktualnych stron + liczba stron */ }

impl LazyPages {
    /// Obszar `len` bajtów; wszystkie strony świeże. Liczba stron = len.div_ceil(PAGE) (ostatnia może być niepełna).
    pub fn new(len: usize) -> LazyPages;
    /// Oznacza jako nieaktualne wszystkie strony, których dotyka którykolwiek z zakresów `(offset, długość)` w bajtach.
    /// Zakresy mogą się nakładać, mieć długość 0 (ignoruj) i wychodzić poza obszar (przytnij, bez paniki; offset+długość może przepełnić u64).
    pub fn invalidate(&mut self, ranges: &[(u64, u64)]);
    pub fn is_stale(&self, page: usize) -> bool;          // poza zakresem = false
    pub fn stale_count(&self) -> usize;
    /// Błąd strony pod bajtem `offset`: które bajty trzeba pobrać z serwera. Okno = grupa `around` kolejnych stron wyrównana do
    /// `around` (np. around=16 -> strony 0..16, 16..32, ...), zawierająca stronę błędu; `around` to potęga dwójki >= 1.
    /// Zwraca scalone, posortowane zakresy bajtów `[od, do)` WYŁĄCZNIE nieaktualnych stron z tego okna (przycięte do `len`).
    /// Jeśli strona błędu jest świeża (albo `offset` poza obszarem): pusty wektor.
    pub fn plan_fetch(&self, offset: usize, around: usize) -> Vec<(usize, usize)>;
    /// Oznacza strony objęte `[od, do)` jako świeże (zakres przycięty do obszaru; niewyrównany początek/koniec obejmuje całe strony,
    /// z wyjątkiem ostatniej niepełnej strony obszaru, która jest świeża, gdy `do` >= len).
    pub fn mark_fresh(&mut self, from: usize, to: usize);
}
```

Testy (w tym samym pliku, `#[cfg(test)]`), minimum:
1. nowy obszar jest cały świeży; `invalidate` z nakładającymi się, zerowymi i wykraczającymi poza obszar zakresami (w tym `(u64::MAX - 1, 100)`) zaznacza dokładnie oczekiwane strony;
2. `plan_fetch` zwraca tylko nieaktualne strony okna, scalone w zakresy, wyrównane do stron; dla strony świeżej pusty wektor; okno przy końcu obszaru przycięte do `len`;
3. `mark_fresh` + `stale_count` spójne; ostatnia niepełna strona (len nie jest wielokrotnością PAGE);
4. `around = 1` i `around = 16`, obszar jednostronicowy i obszar 0 bajtów (brak paniki).

## Zadanie B: `crates/gpu-icd/src/shadow.rs` (mała warstwa `unsafe`)

Pamięć podwójnie zmapowana: jeden plik anonimowy (`memfd_create`) widoczny w dwóch miejscach: **widok aplikacji** (ten dostaje aplikacja
z `vkMapMemory`; później będzie na nim obsługa błędów stron) i **widok własny** (nim sterownik zawsze bezpiecznie pisze i czyta).

```rust
pub struct DoubleMap { /* fd (OwnedFd), wskaźniki obu widoków, długość */ }

impl DoubleMap {
    /// `len` bajtów (zaokrąglone w górę do 4096; len == 0 -> Err(InvalidInput)). Plik: memfd_create(nazwa, MFD_CLOEXEC), ftruncate,
    /// dwa mmap(MAP_SHARED, PROT_READ|PROT_WRITE) tego pliku. Zero na starcie (jak każdy nowy memfd).
    pub fn new(len: usize) -> std::io::Result<DoubleMap>;
    pub fn len(&self) -> usize;                 // po zaokrągleniu
    pub fn app_ptr(&self) -> *mut u8;           // widok aplikacji
    pub fn own_ptr(&self) -> *mut u8;           // widok własny
    /// Zrzuca tablice stron widoku aplikacji dla `[offset, offset+len)` (madvise MADV_DONTNEED): dane w pliku zostają, ale
    /// następny dostęp aplikacji do tych stron wywoła ponowne mapowanie (u nas: błąd strony). offset i len muszą być wielokrotnościami 4096
    /// i mieścić się w obszarze, inaczej Err(InvalidInput).
    pub fn zap_app_view(&self, offset: usize, len: usize) -> std::io::Result<()>;
}
// Drop: munmap obu widoków (zamknięcie fd przez OwnedFd).
```

Wymagane stałe Linuksa (x86_64): `MFD_CLOEXEC = 1`, `PROT_READ = 1`, `PROT_WRITE = 2`, `MAP_SHARED = 1`, `MADV_DONTNEED = 4`,
`SYS_memfd_create = 319` (przez `syscall`, jak `SYS_USERFAULTFD` w memtrack.rs) albo `memfd_create` z libc (deklaracja `extern "C"`).
`#[derive]`/`Send`: zaimplementuj `unsafe impl Send for DoubleMap {}` i `Sync` z komentarzem uzasadniającym (wskaźniki do mapowań żyjących
tak długo jak struktura; synchronizację dostępu do danych zapewnia użytkownik).

Testy (`#[cfg(test)]`), minimum:
1. zapis przez widok własny jest widoczny w widoku aplikacji i odwrotnie (różne adresy, ta sama zawartość);
2. `len` zaokrąglane do 4096; `new(0)` zwraca błąd;
3. po `zap_app_view` dane nadal czytelne przez widok aplikacji (strony wracają z pliku) i widok własny nie traci zawartości;
4. `zap_app_view` z niewyrównanym offset/len albo poza obszarem zwraca błąd (bez paniki);
5. po `drop` kolejne `new` działa (brak wycieku deskryptorów: utwórz i zniszcz 200 obiektów w pętli).
