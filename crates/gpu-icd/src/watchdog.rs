//! Diagnostyka zawieszeń: każdy wątek zapisuje, co robi w sterowniku (polecenie i faza), a wątek nadzorujący, gdy przez 8 s nic się nie rusza,
//! wypisuje do logu stan wszystkich wątków. Bez tego zawieszenie gry (jedna blokada, bilety odpowiedzi, wątek odczytu leniwego) jest nie do
//! zdiagnozowania: `gdb -p` jest zablokowane (`ptrace_scope=1`), a log milknie.

use std::cell::OnceCell;
use std::sync::atomic::{AtomicU16, AtomicU64, AtomicU8, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Fazy wejścia wątku do sterownika.
pub const IDLE: u8 = 0;
/// Przed blokadą instancji (czeka na nią).
pub const WAIT_LOCK: u8 = 1;
/// Trzyma blokadę instancji.
pub const HOLD_LOCK: u8 = 2;
/// Czeka na odpowiedź serwera (bez blokady), parametr = bilet.
pub const WAIT_REPLY: u8 = 3;
/// Odpowiedź jest, czeka na blokadę instancji, żeby ją obrobić.
pub const WAIT_LOCK_FINISH: u8 = 4;
/// Wątek obsługi odczytu leniwego pobiera okno stron.
pub const LAZY_FETCH: u8 = 5;

pub struct Act {
    name: String,
    op: AtomicU16,
    phase: AtomicU8,
    ticket: AtomicU64,
    /// Milisekundy od startu procesu, kiedy wątek wszedł w obecną fazę.
    since: AtomicU64,
}

static REGISTRY: Mutex<Vec<Arc<Act>>> = Mutex::new(Vec::new());
static START: OnceLock<Instant> = OnceLock::new();
/// Licznik postępu: rośnie przy każdym zakończonym poleceniu (ruch = brak zawieszenia).
pub static PROGRESS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

thread_local! {
    static ME: OnceCell<Arc<Act>> = const { OnceCell::new() };
}

fn me<R>(f: impl FnOnce(&Act) -> R) -> R {
    ME.with(|c| {
        let a = c.get_or_init(|| {
            let t = std::thread::current();
            let act = Arc::new(Act { name: t.name().map_or_else(|| format!("{:?}", t.id()), |n| n.to_string()), op: AtomicU16::new(0), phase: AtomicU8::new(IDLE), ticket: AtomicU64::new(0), since: AtomicU64::new(0) });
            REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).push(act.clone());
            act
        });
        f(a)
    })
}

/// Ustawia fazę i polecenie bieżącego wątku (tanie: kilka zapisów atomowych).
pub fn set(op: u16, phase: u8, ticket: u64) {
    me(|a| {
        a.op.store(op, Relaxed);
        a.ticket.store(ticket, Relaxed);
        a.since.store(now_ms(), Relaxed);
        a.phase.store(phase, Relaxed);
    });
}

/// Zmiana fazy z zachowaniem polecenia.
pub fn phase(phase: u8, ticket: u64) {
    me(|a| {
        a.ticket.store(ticket, Relaxed);
        a.since.store(now_ms(), Relaxed);
        a.phase.store(phase, Relaxed);
    });
}

pub fn done() {
    PROGRESS.fetch_add(1, Relaxed);
    me(|a| a.phase.store(IDLE, Relaxed));
}

fn phase_name(p: u8) -> &'static str {
    match p {
        IDLE => "bezczynny",
        WAIT_LOCK => "czeka na blokadę instancji",
        HOLD_LOCK => "TRZYMA blokadę instancji",
        WAIT_REPLY => "czeka na odpowiedź serwera (bilet)",
        WAIT_LOCK_FINISH => "ma odpowiedź, czeka na blokadę instancji",
        LAZY_FETCH => "pobiera okno leniwej pamięci",
        _ => "?",
    }
}

/// Uruchamia wątek nadzorujący (raz na proces). `link_state` zwraca opis stanu połączenia (odczytane / wysłane odpowiedzi).
pub fn start(link_state: fn() -> String, extra_progress: fn() -> u64) {
    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_err() {
        return;
    }
    let _ = std::thread::Builder::new().name("gpu-icd-watchdog".into()).spawn(move || {
        let (mut last, mut since, mut reported) = (PROGRESS.load(Relaxed) + extra_progress(), Instant::now(), false);
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let p = PROGRESS.load(Relaxed) + extra_progress();
            if p != last {
                (last, since, reported) = (p, Instant::now(), false);
                continue;
            }
            let busy = REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|a| a.phase.load(Relaxed) != IDLE);
            if !busy || reported || since.elapsed() < Duration::from_secs(8) {
                continue;
            }
            reported = true;
            let now = now_ms();
            eprintln!("[gpu-icd] WATCHDOG: brak postępu od {} s; stan wątków ({}):", since.elapsed().as_secs(), link_state());
            for a in REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                let ph = a.phase.load(Relaxed);
                if ph == IDLE {
                    continue;
                }
                let op = a.op.load(Relaxed);
                let name = gpu_proto::meta::COMMANDS.get(op as usize).map_or("?", |c| c.name);
                eprintln!("[gpu-icd]   wątek {:<18} {} od {} ms, polecenie {name}, bilet {}", a.name, phase_name(ph), now.saturating_sub(a.since.load(Relaxed)), a.ticket.load(Relaxed));
            }
        }
    });
}
