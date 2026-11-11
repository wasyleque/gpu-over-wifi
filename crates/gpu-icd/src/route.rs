//! Trasa aplikacji: która karta ją obsługuje, lokalna (laptopa) czy zdalna (`gpu-renderd`). Projekt: `docs/DESIGN-route.md`.
//!
//! Decyzja zapada RAZ na proces, przy pierwszym wejściu loadera do ICD, bo stanu Vulkana nie da się dzielić między dwie karty
//! ani przenosić w trakcie działania. Trasa lokalna to trampolina: `vk_icd*` oddają wskaźniki lokalnego sterownika, więc
//! aplikacja rozmawia z nim bezpośrednio (jedyny narzut: licznik klatek na `vkQueuePresentKHR`).
//!
//! `GPU_ICD_ROUTE`: brak zmiennej = moduł wyłączony (zachowanie jak dotąd); `remote`/`local` = wymuszenie z pomiarem;
//! `auto` = wybór z pomiarów: karta lokalna, dopóki spełnia cel (`GPU_ICD_ROUTE_TARGET_FPS`, domyślnie 60), zdalna tylko
//! wtedy, gdy lokalna nie wystarcza, a pomiar pokazał, że zdalna jest szybsza. Miarą jest czas klatki ważony czasem gry
//! (75. percentyl), zbierany w histogramie i sumowany między uruchomieniami w pliku profilu aplikacji.
//! Pozostałe zmienne: `GPU_ICD_ROUTE_KEY` (nazwa profilu), `GPU_ICD_ROUTE_DIR` (katalog profili), `GPU_ICD_ROUTE_PROBE_MS`
//! (limit sondy serwera, domyślnie 300), `GPU_ICD_ROUTE_RELEARN=1` (porzuca profil aplikacji).

use std::ffi::{c_char, c_void, CStr, CString};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Local,
    Remote,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Policy {
    Local,
    Remote,
    Auto,
}

/// `None`: zmienna nieustawiona (moduł wyłączony). Nieznana wartość znaczy `remote`, czyli dotychczasowe zachowanie.
pub fn parse_policy(v: Option<&str>) -> Option<Policy> {
    Some(match v?.trim().to_ascii_lowercase().as_str() {
        "auto" => Policy::Auto,
        "local" | "lokalnie" => Policy::Local,
        _ => Policy::Remote,
    })
}

// ---------------------------------------------------------------------------------------------------------------------
// Pomiar: histogram odstępów między klatkami

/// Przedziały co 1/8 oktawy (szerokość ok. 9%, czyli poniżej rozrzutu pomiarów ±6%) od 0,5 ms do ok. 2 s.
pub const BUCKETS: usize = 96;
const MIN_US: u64 = 500;
/// Dłuższe odstępy to pauza, ekran ładowania albo zmiana trybu: nie są czasem klatki.
const MAX_FRAME_US: u64 = 1_000_000;
/// Początek uruchomienia (kompilacja shaderów, intro) nie wchodzi do pomiaru.
const WARMUP_US: u64 = 5_000_000;
/// Poniżej tej ilości danych profil nie decyduje (i krótkie uruchomienie nie jest zapisywane).
pub const MIN_TOTAL_US: u64 = 60_000_000;
pub const MIN_FRAMES: u64 = 600;
/// Powyżej tej sumy histogram jest połowiony, żeby nowe pomiary nie ginęły w starych.
const DECAY_ABOVE_US: u64 = 4 * 3600 * 1_000_000;
/// Zdalna karta wygrywa dopiero z taką przewagą (rozrzut pomiarów ±6% plus dodatkowe opóźnienie obrazu trasy zdalnej).
const REMOTE_MARGIN_PCT: u64 = 10;

fn bucket_of(dt_us: u64) -> usize {
    let x = (dt_us.max(MIN_US) as f64 / MIN_US as f64).log2() * 8.0;
    (x as usize).min(BUCKETS - 1)
}

fn bucket_center_us(i: usize) -> u64 {
    (MIN_US as f64 * 2f64.powf((i as f64 + 0.5) / 8.0)).round() as u64
}

/// Histogram czasu klatki: `us[i]` to ŁĄCZNY czas (µs) klatek z przedziału `i`, więc percentyle są ważone czasem gry,
/// nie liczbą klatek (minuta menu przy 300 FPS waży tyle co minuta sceny przy 20 FPS).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Hist {
    pub us: [u64; BUCKETS],
    pub frames: u64,
}

impl Default for Hist {
    fn default() -> Self {
        Hist { us: [0; BUCKETS], frames: 0 }
    }
}

impl Hist {
    pub fn add(&mut self, dt_us: u64) {
        self.us[bucket_of(dt_us)] += dt_us;
        self.frames += 1;
    }

    pub fn total_us(&self) -> u64 {
        self.us.iter().sum()
    }

    pub fn sufficient(&self) -> bool {
        self.total_us() >= MIN_TOTAL_US && self.frames >= MIN_FRAMES
    }

    /// Czas klatki (µs), poniżej którego mieści się `pct` procent czasu gry; `None` dla pustego histogramu.
    pub fn percentile_us(&self, pct: u64) -> Option<u64> {
        let total = self.total_us();
        if total == 0 {
            return None;
        }
        let want = (total as u128 * pct.min(100) as u128).div_ceil(100) as u64;
        let mut acc = 0u64;
        for (i, &t) in self.us.iter().enumerate() {
            acc += t;
            if t > 0 && acc >= want {
                return Some(bucket_center_us(i));
            }
        }
        None
    }

    pub fn merge(&mut self, other: &Hist) {
        for (a, b) in self.us.iter_mut().zip(other.us.iter()) {
            *a += *b;
        }
        self.frames += other.frames;
        while self.total_us() > DECAY_ABOVE_US {
            self.us.iter_mut().for_each(|t| *t /= 2);
            self.frames /= 2;
        }
    }
}

/// Licznik klatek jednego uruchomienia. Czas podawany z zewnątrz (µs od dowolnego początku), żeby dało się testować.
#[derive(Default)]
pub struct Meter {
    first: Option<u64>,
    last: Option<u64>,
    pub run: Hist,
}

impl Meter {
    pub fn tick(&mut self, now_us: u64) {
        let first = *self.first.get_or_insert(now_us);
        if now_us.saturating_sub(first) >= WARMUP_US {
            if let Some(last) = self.last {
                let dt = now_us.saturating_sub(last);
                if dt > 0 && dt <= MAX_FRAME_US {
                    self.run.add(dt);
                }
            }
        }
        self.last = Some(now_us);
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Profil aplikacji (plik tekstowy, po jednym wierszu na trasę)

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    /// Wersja `gpu-icd`, która zebrała pomiar. Dla trasy zdalnej pomiar z innej wersji jest nieważny (jej wydajność
    /// zmienia się z każdą optymalizacją sterownika); dla lokalnej wersja nie ma znaczenia.
    pub version: String,
    pub unix: u64,
    pub hist: Hist,
}

#[derive(Default, Clone, PartialEq, Eq, Debug)]
pub struct Profile {
    pub local: Option<Entry>,
    pub remote: Option<Entry>,
}

impl Profile {
    pub fn entry(&self, mode: Mode) -> Option<&Entry> {
        match mode {
            Mode::Local => self.local.as_ref(),
            Mode::Remote => self.remote.as_ref(),
        }
    }

    fn slot(&mut self, mode: Mode) -> &mut Option<Entry> {
        match mode {
            Mode::Local => &mut self.local,
            Mode::Remote => &mut self.remote,
        }
    }

    /// Histogram, na którym wolno oprzeć decyzję i do którego wolno dopisywać: dla trasy zdalnej tylko z bieżącej wersji.
    pub fn usable(&self, mode: Mode, version: &str) -> Option<&Hist> {
        self.entry(mode).filter(|e| mode == Mode::Local || e.version == version).map(|e| &e.hist)
    }

    /// 75. percentyl czasu klatki trasy, jeśli danych jest dość, żeby decydować.
    pub fn p75_us(&self, mode: Mode, version: &str) -> Option<u64> {
        self.usable(mode, version).filter(|h| h.sufficient()).and_then(|h| h.percentile_us(75))
    }

    pub fn parse(text: &str) -> Profile {
        let mut p = Profile::default();
        for line in text.lines() {
            let mut words = line.split_whitespace();
            let mode = match words.next() {
                Some("local") => Mode::Local,
                Some("remote") => Mode::Remote,
                _ => continue,
            };
            if let Some(e) = parse_entry(words) {
                *p.slot(mode) = Some(e);
            }
        }
        p
    }

    pub fn format(&self) -> String {
        let mut out = String::from("# gpu-icd: profil trasy (czas klatek w mikrosekundach na przedział histogramu)\n");
        for (name, e) in [("local", &self.local), ("remote", &self.remote)] {
            let Some(e) = e else { continue };
            let h: Vec<String> = e.hist.us.iter().enumerate().filter(|(_, &t)| t > 0).map(|(i, t)| format!("{i}:{t}")).collect();
            out.push_str(&format!("{name} v={} t={} frames={} h={}\n", e.version, e.unix, e.hist.frames, h.join(",")));
        }
        out
    }
}

fn parse_entry<'a>(words: impl Iterator<Item = &'a str>) -> Option<Entry> {
    let (mut version, mut unix, mut frames, mut hist) = (None, None, None, None);
    for w in words {
        let (k, v) = w.split_once('=')?;
        match k {
            "v" => version = Some(v.to_string()),
            "t" => unix = v.parse::<u64>().ok(),
            "frames" => frames = v.parse::<u64>().ok(),
            "h" => {
                let mut h = Hist::default();
                for pair in v.split(',').filter(|p| !p.is_empty()) {
                    let (i, t) = pair.split_once(':')?;
                    *h.us.get_mut(i.parse::<usize>().ok()?)? = t.parse().ok()?;
                }
                hist = Some(h);
            }
            _ => {}
        }
    }
    let mut hist = hist?;
    hist.frames = frames?;
    Some(Entry { version: version?, unix: unix?, hist })
}

/// Nazwa profilu: małe litery, cyfry, `.`, `_`, `-`; reszta zamieniona na `_`, najwyżej 64 znaki.
pub fn sanitize_key(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let mut k: String = base.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c.to_ascii_lowercase() } else { '_' }).take(64).collect();
    if k.trim_matches(['.', '_']).is_empty() {
        k = "unknown".into();
    }
    k
}

/// Klucz profilu z dostępnych źródeł, od najpewniejszego: jawny, identyfikator gry Steam, nazwa pliku z wiersza poleceń
/// (pod Wine to ścieżka Windows do pliku gry), nazwa procesu.
pub fn key_from(explicit: Option<&str>, steam_app: Option<&str>, argv0: Option<&str>, comm: Option<&str>) -> String {
    if let Some(k) = explicit.filter(|k| !k.trim().is_empty()) {
        return sanitize_key(k.trim());
    }
    if let Some(id) = steam_app.map(str::trim).filter(|id| !id.is_empty() && *id != "0" && id.bytes().all(|b| b.is_ascii_digit())) {
        return format!("steam-{id}");
    }
    sanitize_key(argv0.or(comm).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("unknown"))
}

// ---------------------------------------------------------------------------------------------------------------------
// Decyzja (czysta funkcja)

pub struct Inputs {
    pub policy: Policy,
    /// Lokalny sterownik dał się załadować.
    pub local_ok: bool,
    /// Serwer odpowiada (i jest konfiguracja połączenia).
    pub remote_ok: bool,
    pub local_p75_us: Option<u64>,
    pub remote_p75_us: Option<u64>,
    /// Czas klatki odpowiadający celowi FPS.
    pub target_us: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    Forced,
    NoLocalDriver,
    ServerUnreachable,
    /// Brak pomiaru karty lokalnej: zaczynamy od niej (tańsza: bez sieci i bez serwera).
    ExploreLocal,
    LocalMeetsTarget,
    /// Lokalna za wolna, a zdalna jeszcze niezmierzona w tej wersji sterownika.
    ExploreRemote,
    RemoteFaster,
    LocalNotSlower,
}

pub fn decide(i: &Inputs) -> (Mode, Reason) {
    match i.policy {
        Policy::Remote => return (Mode::Remote, Reason::Forced),
        Policy::Local if i.local_ok => return (Mode::Local, Reason::Forced),
        Policy::Local => return (Mode::Remote, Reason::NoLocalDriver),
        Policy::Auto => {}
    }
    if !i.local_ok {
        return (Mode::Remote, Reason::NoLocalDriver);
    }
    if !i.remote_ok {
        return (Mode::Local, Reason::ServerUnreachable);
    }
    let Some(local) = i.local_p75_us else { return (Mode::Local, Reason::ExploreLocal) };
    if local <= i.target_us {
        return (Mode::Local, Reason::LocalMeetsTarget);
    }
    match i.remote_p75_us {
        None => (Mode::Remote, Reason::ExploreRemote),
        Some(remote) if remote as u128 * (100 + REMOTE_MARGIN_PCT) as u128 <= local as u128 * 100 => (Mode::Remote, Reason::RemoteFaster),
        Some(_) => (Mode::Local, Reason::LocalNotSlower),
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Zapis profilu w trakcie gry

/// Co ile zapisujemy profil, gdy uruchomienie ma już dość danych (gry bywają zabijane, więc nie czekamy na koniec procesu).
const FLUSH_EVERY_US: u64 = 15_000_000;

struct Recorder {
    mode: Mode,
    path: PathBuf,
    /// Histogram tej trasy wczytany przy starcie (już po odrzuceniu nieważnej wersji): do niego dopisujemy bieżące uruchomienie.
    base: Hist,
    meter: Meter,
    start: Instant,
    last_flush_us: u64,
}

impl Recorder {
    fn tick(&mut self) {
        let now = self.start.elapsed().as_micros() as u64;
        self.meter.tick(now);
        if self.meter.run.sufficient() && now.saturating_sub(self.last_flush_us) >= FLUSH_EVERY_US {
            self.last_flush_us = now;
            if let Err(e) = flush(&self.path, self.mode, &self.base, &self.meter.run) {
                eprintln!("[gpu-icd] trasa: zapis profilu {} nieudany: {e}", self.path.display());
            }
        }
    }
}

/// Zapisuje `base + run` jako wiersz trasy `mode`; wiersz drugiej trasy bierze z pliku (mógł go w międzyczasie zmienić inny proces).
pub fn flush(path: &Path, mode: Mode, base: &Hist, run: &Hist) -> std::io::Result<()> {
    let mut profile = std::fs::read_to_string(path).map(|t| Profile::parse(&t)).unwrap_or_default();
    let mut hist = base.clone();
    hist.merge(run);
    let unix = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    *profile.slot(mode) = Some(Entry { version: env!("CARGO_PKG_VERSION").to_string(), unix, hist });
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, profile.format())?;
    std::fs::rename(&tmp, path)
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static RECORDER: Mutex<Option<Recorder>> = Mutex::new(None);

/// Wołane przy każdym `vkQueuePresentKHR` (obie trasy). Bez `GPU_ICD_ROUTE` nie robi nic.
pub fn frame_tick() {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    if let Some(r) = RECORDER.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        r.tick();
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Trampolina do lokalnego sterownika

type Gipa = unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_void;
type Negotiate = unsafe extern "C" fn(*mut u32) -> i32;
type Present = unsafe extern "C" fn(*mut c_void, *const c_void) -> i32;

/// Punkty wejścia lokalnego sterownika ICD. Żyją do końca procesu (biblioteki nie rozładowujemy, jak loader).
pub struct Passthrough {
    gipa: Gipa,
    gpdpa: Option<Gipa>,
    negotiate: Option<Negotiate>,
}

/// Prawdziwe `vkGetDeviceProcAddr` i `vkQueuePresentKHR` lokalnego sterownika. Jeden sterownik w procesie, więc wystarczą
/// pojedyncze wskaźniki (dla każdego urządzenia tego sterownika są to te same funkcje).
static REAL_GDPA: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REAL_PRESENT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

impl Passthrough {
    /// # Safety
    /// Jak `vk_icdGetInstanceProcAddr`: `name` to napis C, `instance` jest null albo uchwytem lokalnego sterownika.
    pub unsafe fn instance_proc(&self, instance: *mut c_void, name: *const c_char) -> *const c_void {
        hook(name, (self.gipa)(instance, name))
    }

    /// # Safety
    /// Jak `vk_icdGetPhysicalDeviceProcAddr`.
    pub unsafe fn physical_device_proc(&self, instance: *mut c_void, name: *const c_char) -> *const c_void {
        match self.gpdpa {
            Some(f) => f(instance, name),
            None => std::ptr::null(),
        }
    }

    /// # Safety
    /// `version` jest null albo wskazuje na `u32`.
    pub unsafe fn negotiate(&self, version: *mut u32) -> i32 {
        match (self.negotiate, version.as_mut()) {
            (Some(f), _) => f(version),
            // Sterownik bez funkcji negocjacji mówi interfejsem w wersji najwyżej 1.
            (None, Some(v)) => {
                *v = (*v).min(1);
                0
            }
            (None, None) => -3,
        }
    }
}

/// Podmienia dwie funkcje, przez które liczymy klatki; resztę oddaje bez zmian.
unsafe fn hook(name: *const c_char, real: *const c_void) -> *const c_void {
    if real.is_null() || name.is_null() {
        return real;
    }
    match CStr::from_ptr(name).to_bytes() {
        b"vkGetDeviceProcAddr" => {
            REAL_GDPA.store(real as *mut c_void, Ordering::SeqCst);
            gdpa_hook as Gipa as *const c_void
        }
        b"vkQueuePresentKHR" => {
            REAL_PRESENT.store(real as *mut c_void, Ordering::SeqCst);
            present_hook as Present as *const c_void
        }
        _ => real,
    }
}

unsafe extern "C" fn gdpa_hook(device: *mut c_void, name: *const c_char) -> *const c_void {
    let real = REAL_GDPA.load(Ordering::SeqCst);
    if real.is_null() {
        return std::ptr::null();
    }
    // SAFETY: wskaźnik zapisał `hook` z odpowiedzi sterownika na "vkGetDeviceProcAddr", więc ma tę sygnaturę.
    let gdpa = std::mem::transmute::<*mut c_void, Gipa>(real);
    hook(name, gdpa(device, name))
}

unsafe extern "C" fn present_hook(queue: *mut c_void, info: *const c_void) -> i32 {
    frame_tick();
    // SAFETY: `present_hook` jest wydawany tylko po zapisaniu niepustego wskaźnika sterownika na "vkQueuePresentKHR".
    let present = std::mem::transmute::<*mut c_void, Present>(REAL_PRESENT.load(Ordering::SeqCst));
    present(queue, info)
}

extern "C" {
    fn dlopen(filename: *const c_char, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
const RTLD_NOW: i32 = 2;

/// Ładuje pierwszy lokalny sterownik fizycznej karty (kolejność jak przy prezentacji: Intel, AMD, NVIDIA).
fn load_local_driver() -> Result<Passthrough, String> {
    let mut errors = Vec::new();
    for manifest in crate::local::find_local_manifests() {
        let Ok(json) = std::fs::read_to_string(&manifest) else { continue };
        let Some(lib) = crate::local::library_path_from_manifest(&json, manifest.parent().unwrap_or(Path::new("/"))) else { continue };
        for cand in crate::local::library_candidates(&lib) {
            let Ok(c) = CString::new(cand.to_string_lossy().as_bytes()) else { continue };
            // SAFETY: `c` to napis C; biblioteka zostaje w procesie na stałe (bez dlclose, jak w loaderze).
            let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW) };
            if handle.is_null() {
                errors.push(format!("{}: dlopen", cand.display()));
                continue;
            }
            // SAFETY: `handle` jest ważny; symbole mają sygnatury z interfejsu loader-ICD.
            let sym = |n: &CStr| unsafe { dlsym(handle, n.as_ptr()) };
            let gipa = sym(c"vk_icdGetInstanceProcAddr");
            if gipa.is_null() {
                errors.push(format!("{}: brak vk_icdGetInstanceProcAddr", cand.display()));
                continue;
            }
            let (gpdpa, neg) = (sym(c"vk_icdGetPhysicalDeviceProcAddr"), sym(c"vk_icdNegotiateLoaderICDInterfaceVersion"));
            // SAFETY: niepuste wskaźniki z dlsym na funkcje o sygnaturach interfejsu ICD.
            return Ok(unsafe {
                Passthrough {
                    gipa: std::mem::transmute::<*mut c_void, Gipa>(gipa),
                    gpdpa: (!gpdpa.is_null()).then(|| std::mem::transmute::<*mut c_void, Gipa>(gpdpa)),
                    negotiate: (!neg.is_null()).then(|| std::mem::transmute::<*mut c_void, Negotiate>(neg)),
                }
            });
        }
    }
    Err(if errors.is_empty() { "nie znaleziono manifestu lokalnego sterownika".into() } else { errors.join(" | ") })
}

// ---------------------------------------------------------------------------------------------------------------------
// Środowisko procesu i jednorazowa decyzja

pub fn profile_dir() -> PathBuf {
    if let Ok(d) = std::env::var("GPU_ICD_ROUTE_DIR") {
        return PathBuf::from(d);
    }
    let state = std::env::var("XDG_STATE_HOME").ok().filter(|s| !s.is_empty()).map(PathBuf::from);
    state.or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local/state"))).unwrap_or_else(|| PathBuf::from("/tmp")).join("gpu-hub/route")
}

pub fn profile_path(key: &str) -> PathBuf {
    profile_dir().join(format!("{key}.route"))
}

fn app_key() -> String {
    let env = |k: &str| std::env::var(k).ok();
    let argv0 = std::fs::read("/proc/self/cmdline").ok().and_then(|b| b.split(|&c| c == 0).next().map(|a| String::from_utf8_lossy(a).into_owned()));
    let comm = std::fs::read_to_string("/proc/self/comm").ok();
    key_from(env("GPU_ICD_ROUTE_KEY").as_deref(), env("SteamAppId").or_else(|| env("SteamGameId")).as_deref(), argv0.as_deref(), comm.as_deref())
}

/// Czy demon przyjmuje połączenia (i czy w ogóle jest skonfigurowany). Samo połączenie TCP, bez logowania.
fn remote_reachable() -> bool {
    let (Ok(addr), Some(_)) = (std::env::var("GPU_ICD_ADDR"), crate::config_token()) else { return false };
    let ms = std::env::var("GPU_ICD_ROUTE_PROBE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(300u64);
    let Some(sock) = addr.to_socket_addrs().ok().and_then(|mut a| a.next()) else { return false };
    TcpStream::connect_timeout(&sock, Duration::from_millis(ms)).is_ok()
}

fn fps(us: Option<u64>) -> String {
    match us {
        Some(us) if us > 0 => format!("{:.1} FPS", 1e6 / us as f64),
        _ => "brak pomiaru".into(),
    }
}

fn init() -> Option<Passthrough> {
    let policy = parse_policy(std::env::var("GPU_ICD_ROUTE").ok().as_deref())?;
    crate::redirect_stderr_to_log();
    let (key, version) = (app_key(), env!("CARGO_PKG_VERSION"));
    let path = profile_path(&key);
    if std::env::var("GPU_ICD_ROUTE_RELEARN").is_ok() {
        let _ = std::fs::remove_file(&path);
    }
    let profile = std::fs::read_to_string(&path).map(|t| Profile::parse(&t)).unwrap_or_default();
    let local = if policy == Policy::Remote { Err("nie sprawdzano".to_string()) } else { load_local_driver() };
    let target_fps = std::env::var("GPU_ICD_ROUTE_TARGET_FPS").ok().and_then(|v| v.parse::<u64>().ok()).filter(|f| (1..=1000).contains(f)).unwrap_or(60);
    let inputs = Inputs {
        policy,
        local_ok: local.is_ok(),
        remote_ok: policy != Policy::Auto || remote_reachable(),
        local_p75_us: profile.p75_us(Mode::Local, version),
        remote_p75_us: profile.p75_us(Mode::Remote, version),
        target_us: 1_000_000 / target_fps,
    };
    let (mode, reason) = decide(&inputs);
    let detail = if let Err(e) = &local { format!("; lokalny sterownik: {e}") } else { String::new() };
    eprintln!(
        "[gpu-icd] trasa: {mode:?} ({reason:?}), profil {key}: lokalnie {}, zdalnie {}, cel {target_fps} FPS{detail}",
        fps(inputs.local_p75_us),
        fps(inputs.remote_p75_us)
    );
    let base = profile.usable(mode, version).cloned().unwrap_or_default();
    *RECORDER.lock().unwrap_or_else(|e| e.into_inner()) = Some(Recorder { mode, path, base, meter: Meter::default(), start: Instant::now(), last_flush_us: 0 });
    ACTIVE.store(true, Ordering::Relaxed);
    match mode {
        Mode::Local => local.ok(),
        Mode::Remote => None,
    }
}

/// Lokalny sterownik, jeśli ten proces idzie trasą lokalną. Pierwsze wywołanie podejmuje decyzję.
pub fn passthrough() -> Option<&'static Passthrough> {
    static CELL: OnceLock<Option<Passthrough>> = OnceLock::new();
    CELL.get_or_init(init).as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto(local: Option<u64>, remote: Option<u64>) -> Inputs {
        Inputs { policy: Policy::Auto, local_ok: true, remote_ok: true, local_p75_us: local, remote_p75_us: remote, target_us: 16_666 }
    }

    #[test]
    fn policy_parsing_keeps_old_behaviour_by_default() {
        assert_eq!(parse_policy(None), None);
        assert_eq!(parse_policy(Some("auto")), Some(Policy::Auto));
        assert_eq!(parse_policy(Some(" Local ")), Some(Policy::Local));
        assert_eq!(parse_policy(Some("remote")), Some(Policy::Remote));
        assert_eq!(parse_policy(Some("cokolwiek")), Some(Policy::Remote));
    }

    #[test]
    fn forced_policies_and_fallbacks() {
        let mut i = auto(Some(1), Some(1));
        i.policy = Policy::Remote;
        assert_eq!(decide(&i), (Mode::Remote, Reason::Forced));
        i.policy = Policy::Local;
        assert_eq!(decide(&i), (Mode::Local, Reason::Forced));
        i.local_ok = false;
        assert_eq!(decide(&i), (Mode::Remote, Reason::NoLocalDriver));
        i.policy = Policy::Auto;
        assert_eq!(decide(&i), (Mode::Remote, Reason::NoLocalDriver));
        let mut i = auto(Some(50_000), Some(10_000));
        i.remote_ok = false;
        assert_eq!(decide(&i), (Mode::Local, Reason::ServerUnreachable));
    }

    #[test]
    fn auto_prefers_local_until_it_is_measured_too_slow() {
        assert_eq!(decide(&auto(None, None)), (Mode::Local, Reason::ExploreLocal));
        // Zdalny pomiar bez lokalnego niczego nie przesądza: lokalna karta może wystarczać.
        assert_eq!(decide(&auto(None, Some(5_000))), (Mode::Local, Reason::ExploreLocal));
        assert_eq!(decide(&auto(Some(16_000), None)), (Mode::Local, Reason::LocalMeetsTarget));
        // Lokalna spełnia cel: zdalna nie jest brana, nawet gdy jest szybsza.
        assert_eq!(decide(&auto(Some(16_000), Some(4_000))), (Mode::Local, Reason::LocalMeetsTarget));
        assert_eq!(decide(&auto(Some(50_000), None)), (Mode::Remote, Reason::ExploreRemote));
    }

    #[test]
    fn auto_takes_remote_only_with_a_margin() {
        assert_eq!(decide(&auto(Some(50_000), Some(38_000))), (Mode::Remote, Reason::RemoteFaster));
        // 110% z 45,5 ms to 50,05 ms > 50 ms: przewaga poniżej progu, zostaje karta lokalna.
        assert_eq!(decide(&auto(Some(50_000), Some(45_500))), (Mode::Local, Reason::LocalNotSlower));
        assert_eq!(decide(&auto(Some(50_000), Some(45_454))), (Mode::Remote, Reason::RemoteFaster));
        assert_eq!(decide(&auto(Some(50_000), Some(80_000))), (Mode::Local, Reason::LocalNotSlower));
    }

    /// Dopuszczalny błąd percentyla: pół szerokości przedziału (ok. 4,5%).
    fn close(got: u64, want: u64) -> bool {
        got.abs_diff(want) * 100 <= want * 5
    }

    #[test]
    fn percentiles_are_weighted_by_time_not_by_frame_count() {
        let mut h = Hist::default();
        (0..12_000).for_each(|_| h.add(5_000)); // 60 s przy 200 FPS
        (0..1_200).for_each(|_| h.add(50_000)); // 60 s przy 20 FPS
        assert_eq!(h.total_us(), 120_000_000);
        assert!(close(h.percentile_us(25).unwrap(), 5_000), "{:?}", h.percentile_us(25));
        assert!(close(h.percentile_us(75).unwrap(), 50_000), "{:?}", h.percentile_us(75));
        // Liczone po klatkach 75. percentyl wypadłby w 5 ms (91% klatek jest szybkich).
        assert!(h.sufficient());
        assert_eq!(Hist::default().percentile_us(75), None);
    }

    #[test]
    fn bucket_centers_stay_within_five_percent_over_the_whole_range() {
        for us in [500u64, 777, 4_167, 6_944, 16_667, 33_333, 100_000, 999_999] {
            assert!(close(bucket_center_us(bucket_of(us)), us), "{us} -> {}", bucket_center_us(bucket_of(us)));
        }
        assert_eq!(bucket_of(1), 0);
        assert_eq!(bucket_of(u64::MAX), BUCKETS - 1);
    }

    #[test]
    fn meter_skips_warmup_pauses_and_needs_enough_data() {
        let mut m = Meter::default();
        let mut t = 0u64;
        // Rozgrzewka: 5 s klatek po 100 ms, które nie mogą wejść do pomiaru.
        while t < WARMUP_US {
            m.tick(t);
            t += 100_000;
        }
        assert_eq!(m.run.frames, 0);
        for _ in 0..3_000 {
            m.tick(t);
            t += 10_000;
        }
        t += 30_000_000; // pauza: pojedynczy odstęp 30 s jest pomijany
        m.tick(t);
        assert!(!m.run.sufficient(), "30 s gry to za mało");
        for _ in 0..3_100 {
            t += 10_000;
            m.tick(t);
        }
        assert!(m.run.sufficient());
        assert!(close(m.run.percentile_us(75).unwrap(), 10_000));
        assert!(m.run.total_us() < 62_000_000, "pauza weszła do pomiaru: {}", m.run.total_us());
    }

    fn hist_of(dt: u64, n: u64) -> Hist {
        let mut h = Hist::default();
        (0..n).for_each(|_| h.add(dt));
        h
    }

    #[test]
    fn profile_roundtrip_and_tolerant_parsing() {
        let p = Profile {
            local: Some(Entry { version: "0.3.11".into(), unix: 1_762_900_000, hist: hist_of(40_000, 2_000) }),
            remote: Some(Entry { version: "0.3.11".into(), unix: 1_762_900_500, hist: hist_of(20_000, 4_000) }),
        };
        assert_eq!(Profile::parse(&p.format()), p);
        let only_local = Profile { remote: None, ..p.clone() };
        assert_eq!(Profile::parse(&only_local.format()), only_local);
        // Uszkodzony wiersz jest pomijany, poprawny zostaje.
        let text = format!("remote v=1 t=abc frames=3 h=1:2\nśmieci\n{}", only_local.format());
        assert_eq!(Profile::parse(&text), only_local);
        assert_eq!(Profile::parse("local v=1 t=1 frames=1 h=999:5"), Profile::default());
        assert_eq!(Profile::parse(""), Profile::default());
    }

    #[test]
    fn remote_measurements_expire_with_the_driver_version_local_ones_do_not() {
        let p = Profile {
            local: Some(Entry { version: "0.1.0".into(), unix: 1, hist: hist_of(40_000, 2_000) }),
            remote: Some(Entry { version: "0.1.0".into(), unix: 1, hist: hist_of(20_000, 4_000) }),
        };
        assert!(close(p.p75_us(Mode::Local, "0.3.11").unwrap(), 40_000));
        assert_eq!(p.p75_us(Mode::Remote, "0.3.11"), None);
        assert!(close(p.p75_us(Mode::Remote, "0.1.0").unwrap(), 20_000));
        // Za mało danych: profil istnieje, ale nie decyduje.
        let thin = Profile { local: Some(Entry { version: "x".into(), unix: 1, hist: hist_of(40_000, 100) }), remote: None };
        assert_eq!(thin.p75_us(Mode::Local, "x"), None);
        assert!(thin.usable(Mode::Local, "x").is_some());
    }

    #[test]
    fn merge_adds_runs_and_halves_old_data_above_the_limit() {
        let mut h = hist_of(10_000, 1_000);
        h.merge(&hist_of(20_000, 500));
        assert_eq!((h.frames, h.total_us()), (1_500, 20_000_000));
        let mut big = hist_of(1_000_000, 4 * 3600);
        big.merge(&hist_of(1_000_000, 3600));
        assert!(big.total_us() <= DECAY_ABOVE_US, "{}", big.total_us());
        assert_eq!(big.total_us(), 5 * 3600 * 1_000_000 / 2);
    }

    #[test]
    fn keys_from_explicit_steam_and_windows_paths() {
        assert_eq!(key_from(Some(" Moja Gra "), Some("42"), Some("x"), None), "moja_gra");
        assert_eq!(key_from(None, Some("391220"), Some("Z:\\gry\\ROTTR.exe"), None), "steam-391220");
        assert_eq!(key_from(None, Some("0"), Some("Z:\\home\\u\\Gry\\Rise\\ROTTR.exe"), None), "rottr.exe");
        assert_eq!(key_from(None, None, Some("/usr/bin/vkcube"), Some("vkcube\n")), "vkcube");
        assert_eq!(key_from(None, None, None, Some("Game.exe\n")), "game.exe");
        assert_eq!(key_from(None, None, None, None), "unknown");
        assert_eq!(sanitize_key("../.."), "unknown");
        assert_eq!(sanitize_key(&"a".repeat(200)).len(), 64);
    }

    #[test]
    fn flush_merges_into_the_file_and_keeps_the_other_route() {
        let dir = std::env::temp_dir().join(format!("gpu-icd-route-test-{}", std::process::id()));
        let path = dir.join("gra.route");
        let _ = std::fs::remove_dir_all(&dir);
        flush(&path, Mode::Remote, &Hist::default(), &hist_of(20_000, 4_000)).unwrap();
        flush(&path, Mode::Local, &hist_of(40_000, 1_000), &hist_of(40_000, 1_000)).unwrap();
        let p = Profile::parse(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(p.remote.as_ref().unwrap().hist, hist_of(20_000, 4_000));
        assert_eq!(p.local.as_ref().unwrap().hist, hist_of(40_000, 2_000));
        assert_eq!(p.local.as_ref().unwrap().version, env!("CARGO_PKG_VERSION"));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "plik tymczasowy został");
        let _ = std::fs::remove_dir_all(&dir);
    }

    static CALLS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    unsafe extern "C" fn fake_present(_q: *mut c_void, _i: *const c_void) -> i32 {
        CALLS.fetch_add(1, Ordering::SeqCst);
        7
    }
    unsafe extern "C" fn fake_other() {}
    unsafe extern "C" fn fake_gdpa(_d: *mut c_void, name: *const c_char) -> *const c_void {
        match CStr::from_ptr(name).to_bytes() {
            b"vkQueuePresentKHR" => fake_present as Present as *const c_void,
            b"vkGetDeviceProcAddr" => fake_gdpa as Gipa as *const c_void,
            b"vkDestroyDevice" => fake_other as unsafe extern "C" fn() as *const c_void,
            _ => std::ptr::null(),
        }
    }
    unsafe extern "C" fn fake_gipa(_i: *mut c_void, name: *const c_char) -> *const c_void {
        match CStr::from_ptr(name).to_bytes() {
            b"vkCreateInstance" => fake_other as unsafe extern "C" fn() as *const c_void,
            _ => fake_gdpa(std::ptr::null_mut(), name),
        }
    }

    #[test]
    fn trampoline_forwards_everything_and_counts_presents() {
        let p = Passthrough { gipa: fake_gipa, gpdpa: None, negotiate: None };
        unsafe {
            let null = std::ptr::null_mut();
            assert_eq!(p.instance_proc(null, c"vkCreateInstance".as_ptr()), fake_other as unsafe extern "C" fn() as *const c_void);
            assert!(p.instance_proc(null, c"vkNieMa".as_ptr()).is_null());
            assert!(p.physical_device_proc(null, c"vkCreateInstance".as_ptr()).is_null());
            let mut v = 7u32;
            assert_eq!((p.negotiate(&mut v), v), (0, 1));
            assert_eq!(p.negotiate(std::ptr::null_mut()), -3);

            // Loader bierze vkGetDeviceProcAddr z instancji, a funkcje urządzenia już przez nie.
            let gdpa = p.instance_proc(null, c"vkGetDeviceProcAddr".as_ptr());
            assert_eq!(gdpa, gdpa_hook as Gipa as *const c_void);
            let gdpa = std::mem::transmute::<*const c_void, Gipa>(gdpa);
            assert_eq!(gdpa(null, c"vkDestroyDevice".as_ptr()), fake_other as unsafe extern "C" fn() as *const c_void);
            assert!(gdpa(null, c"vkNieMa".as_ptr()).is_null());
            assert_eq!(gdpa(null, c"vkGetDeviceProcAddr".as_ptr()), gdpa_hook as Gipa as *const c_void);
            let present = gdpa(null, c"vkQueuePresentKHR".as_ptr());
            assert_eq!(present, present_hook as Present as *const c_void);
            let present = std::mem::transmute::<*const c_void, Present>(present);
            let before = CALLS.load(Ordering::SeqCst);
            assert_eq!(present(null, std::ptr::null()), 7);
            assert_eq!(CALLS.load(Ordering::SeqCst), before + 1);
        }
    }
}
