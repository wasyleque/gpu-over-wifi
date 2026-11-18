Mikrobenchmark A6 (2026-11-18): `write()` vs `send(MSG_ZEROCOPY)` po kablu 2,5G minipc -> serwer.
Serwer: `python3 sink.py` (nasłuch 10.42.0.2:9100, odrzuca dane). Klient: `cargo build --release && ./target/release/zc 10.42.0.2:9100 <rozmiar_bufora> <GB> write|zc`.
Mierzy CPU (user+sys procesu, bez softirq) na GB i przepustowość. Wyniki: `docs/OPTIMIZATION-IDEAS.md` (A6).
