//! Kanały ramek obrazu między sesją sterującą a jej połączeniem wideo: sesja tworzy kanał przy starcie, drugie połączenie
//! (`AttachVideo`) odbiera jego koniec. Kanał jest ograniczony, więc wolny odbiorca spowalnia nadawcę (backpressure).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Mutex;

/// Ile ramek może czekać na wysłanie; dalej nadawca (sesja sterująca) blokuje się.
pub const QUEUE_DEPTH: usize = 2;

static NEXT: AtomicU64 = AtomicU64::new(1);
static PENDING: Mutex<Option<HashMap<u64, Receiver<Vec<u8>>>>> = Mutex::new(None);

fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn with_map<R>(f: impl FnOnce(&mut HashMap<u64, Receiver<Vec<u8>>>) -> R) -> R {
    let mut g = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

/// Tworzy kanał dla nowej sesji: zwraca jej identyfikator (niezerowy) i nadajnik.
pub fn new_session() -> (u64, SyncSender<Vec<u8>>) {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    let id = mix(nanos ^ mix(NEXT.fetch_add(1, Ordering::Relaxed))).max(1);
    let (tx, rx) = sync_channel(QUEUE_DEPTH);
    with_map(|m| m.insert(id, rx));
    (id, tx)
}

/// Odbiera koniec kanału sesji `id` (tylko raz; kolejna próba zwraca `None`).
pub fn claim(id: u64) -> Option<Receiver<Vec<u8>>> {
    with_map(|m| m.remove(&id))
}

/// Usuwa nieodebrany kanał sesji (po jej zakończeniu).
pub fn forget(id: u64) {
    with_map(|m| m.remove(&id));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_get_distinct_nonzero_ids_and_a_receiver_can_be_claimed_once() {
        let (a, tx_a) = new_session();
        let (b, _tx_b) = new_session();
        assert!(a != 0 && b != 0 && a != b);
        tx_a.send(vec![1, 2, 3]).unwrap();
        let rx = claim(a).expect("pierwszy odbiór");
        assert_eq!(rx.recv().unwrap(), vec![1, 2, 3]);
        assert!(claim(a).is_none(), "drugi odbiór tego samego kanału jest niemożliwy");
        assert!(claim(0xDEAD).is_none());
        forget(b);
        assert!(claim(b).is_none());
    }

    #[test]
    fn the_queue_is_bounded_so_a_slow_receiver_backpressures_the_sender() {
        let (id, tx) = new_session();
        for i in 0..QUEUE_DEPTH {
            tx.try_send(vec![i as u8]).unwrap();
        }
        assert!(tx.try_send(vec![99]).is_err(), "kolejka pełna");
        let rx = claim(id).unwrap();
        assert_eq!(rx.recv().unwrap(), vec![0]);
        assert!(tx.try_send(vec![99]).is_ok(), "po odebraniu jest miejsce");
    }

    #[test]
    fn dropping_the_sender_ends_the_receiver() {
        let (id, tx) = new_session();
        let rx = claim(id).unwrap();
        drop(tx);
        assert!(rx.recv().is_err());
    }
}
