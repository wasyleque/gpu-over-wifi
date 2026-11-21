//! Kanał odczytów (domyślnie włączony, `GPU_ICD_READCHAN=0` wyłącza): drugie połączenie z demonem, podłączone do tej samej sesji (`AttachReads`), którym pobranie leniwe prosi o dane GPU
//! bez stania w kolejce za zapisami i poleceniami strumienia sterującego. Żądanie niesie numer ramki, po której serwer ma czytać (`Link::read_dep`).
//! Każdy błąd (rozłączenie, brak pamięci w widoku serwera) kończy się powrotem do zwykłego odczytu strumieniem sterującym; po błędzie transportu kanał jest wyłączony.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use gpu_proto::msg::{Request, Response};
use gpu_renderd::client::Client;

pub struct ReadChan {
    /// Sesja serwera, do której kanał jest podłączony: odczyt pamięci innej sesji (inna instancja Vulkana w tym samym procesie) musi iść jej własnym strumieniem.
    session: u64,
    client: Mutex<Client>,
    broken: AtomicBool,
}

/// Od 0.3.26 domyślnie włączony; `GPU_ICD_READCHAN=0` wyłącza. Pomiar CP (pełny ekran, pary naprzemienne): 15,67 / 16,12 FPS z kanałem wobec 15,42 / 15,23 bez
/// (wcześniejsze przebiegi z kanałem 15,48–16,24, bez 15,0–15,6): ok. +0,5 FPS (+3–4%).
pub fn enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var("GPU_ICD_READCHAN").map(|v| v != "0").unwrap_or(true))
}

static CHAN: Mutex<Option<Arc<ReadChan>>> = Mutex::new(None);

/// Podłącza kanał odczytów do sesji `session` (wołane przy tworzeniu swapchaina, gdy znamy identyfikator sesji). Zastępuje poprzedni kanał.
pub fn attach(addr: &str, token: &[u8], session: u64) {
    if !enabled() {
        return;
    }
    let made = (|| -> Result<ReadChan, String> {
        let mut c = Client::connect(addr, token).map_err(|e| format!("{e:?}"))?;
        match c.call(&Request::AttachReads { session }) {
            Ok(Response::U32(0)) => {}
            other => return Err(format!("AttachReads: {other:?}")),
        }
        // Serwer czeka na zależność najwyżej 3 s (było 30 s: odczyt innej sesji wisiał pół minuty); klient trochę dłużej, żeby dostać odpowiedź o przekroczeniu czasu zamiast własnego błędu.
        c.set_read_timeout(Some(Duration::from_secs(6))).map_err(|e| format!("{e:?}"))?;
        Ok(ReadChan { session, client: Mutex::new(c), broken: AtomicBool::new(false) })
    })();
    match made {
        Ok(ch) => {
            eprintln!("[gpu-icd] kanał odczytów podłączony (sesja {session:#x})");
            *CHAN.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(ch));
        }
        Err(e) => eprintln!("[gpu-icd] kanał odczytów niedostępny, odczyty strumieniem sterującym: {e}"),
    }
}

/// Działający kanał dla sesji `session` (albo `None`: niepodłączony, zepsuty albo podłączony do innej sesji).
pub fn get_for(session: u64) -> Option<Arc<ReadChan>> {
    let g = CHAN.lock().unwrap_or_else(|e| e.into_inner());
    if !crate::tuner::readchan() {
        return None;
    }
    g.as_ref().filter(|c| session != 0 && c.session == session && !c.broken.load(Ordering::Relaxed)).cloned()
}

pub static READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static READ_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static FALLBACKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl ReadChan {
    /// Czyta `len` bajtów po ramce `after`. `None` = nie udało się (wołający wraca do strumienia sterującego); błąd transportu wyłącza kanał.
    pub fn read(&self, after: u16, memory: u64, offset: u64, len: usize) -> Option<Vec<u8>> {
        let t = std::time::Instant::now();
        let mut c = self.client.lock().unwrap_or_else(|e| e.into_inner());
        let r = c.call(&Request::MemReadAfter { after: after as u64, memory, offset, len: len as u32 });
        READ_NS.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
        match r {
            Ok(Response::Data(d)) if d.len() == len => {
                READS.fetch_add(1, Ordering::Relaxed);
                Some(d)
            }
            Ok(other) => {
                static SEEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if SEEN.fetch_add(1, Ordering::Relaxed) < 5 {
                    eprintln!("[gpu-icd] kanał odczytów: serwer odmówił odczytu (pamięć {memory:#x}, offset {offset}, długość {len}): {other:?}");
                }
                FALLBACKS.fetch_add(1, Ordering::Relaxed);
                None
            }
            // Odmowa serwera (np. pamięć jeszcze nie odwzorowana w jego widoku: pierwszy odczyt strumieniem sterującym ją odwzorowuje) nie psuje kanału.
            Err(gpu_renderd::client::ClientError::Remote(code, msg)) => {
                static SEEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if SEEN.fetch_add(1, Ordering::Relaxed) < 8 {
                    eprintln!("[gpu-icd] kanał odczytów: serwer odmówił ({code}, {msg}) dla pamięci {memory:#x}, offset {offset}, długość {len}");
                }
                FALLBACKS.fetch_add(1, Ordering::Relaxed);
                None
            }
            Err(e) => {
                eprintln!("[gpu-icd] kanał odczytów: błąd transportu, wyłączony: {e:?}");
                self.broken.store(true, Ordering::Relaxed);
                FALLBACKS.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }
}
