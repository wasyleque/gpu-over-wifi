//! Mikrobenchmark A6: write() vs send(MSG_ZEROCOPY): CPU (user+sys) na GB i przepustowość. Użycie: zc <ip:port> <rozmiar_bufora> <GB> <tryb: write|zc>
use std::io::Write;
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::time::Instant;

fn cpu() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    u.ru_utime.tv_sec as f64 + u.ru_utime.tv_usec as f64 / 1e6 + u.ru_stime.tv_sec as f64 + u.ru_stime.tv_usec as f64 / 1e6
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (addr, size, gb, mode) = (&a[1], a[2].parse::<usize>().unwrap(), a[3].parse::<f64>().unwrap(), a[4].as_str());
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_nodelay(true).unwrap();
    let fd = s.as_raw_fd();
    let n = 8; // bufory w obiegu (przy zerocopy nie wolno ich ruszać do potwierdzenia)
    let mut bufs: Vec<Vec<u8>> = (0..n).map(|i| (0..size).map(|j| ((j * 31 + i) % 251) as u8).collect()).collect();
    let total = (gb * 1e9) as usize;
    if mode == "zc" {
        let one: i32 = 1;
        let r = unsafe { libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_ZEROCOPY, &one as *const _ as *const _, 4) };
        assert_eq!(r, 0, "SO_ZEROCOPY niedostępne");
    }
    let (t0, c0) = (Instant::now(), cpu());
    let (mut sent, mut i, mut pending) = (0usize, 0usize, 0usize);
    while sent < total {
        let b = &mut bufs[i % n];
        if mode == "zc" {
            let mut off = 0;
            while off < size {
                let r = unsafe { libc::send(fd, b[off..].as_ptr() as *const _, size - off, libc::MSG_ZEROCOPY) };
                if r < 0 {
                    let e = std::io::Error::last_os_error();
                    if e.raw_os_error() == Some(libc::ENOBUFS) { reap_wait(fd, &mut pending, 0); continue; }
                    panic!("send: {e}");
                }
                off += r as usize;
            }
            pending += 1;
            if pending >= n { reap_wait(fd, &mut pending, n / 2); }
        } else {
            s.write_all(b).unwrap();
            b[0] = b[0].wrapping_add(1); // symulacja zmiany bufora
        }
        sent += size;
        i += 1;
    }
    if mode == "zc" { reap_wait(fd, &mut pending, 0); }
    let (dt, dc) = (t0.elapsed().as_secs_f64(), cpu() - c0);
    println!("{mode:5} bufor {:>8} B: {:6.1} MB/s, CPU {:5.2} s/GB (CPU {:3.0}% jednego rdzenia)", size, total as f64 / dt / 1e6, dc / (total as f64 / 1e9), dc / dt * 100.0);
}

fn reap(fd: i32, pending: &mut usize) {
    let mut ctrl = [0u8; 128];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_control = ctrl.as_mut_ptr() as *mut _;
    msg.msg_controllen = ctrl.len();
    while unsafe { libc::recvmsg(fd, &mut msg, libc::MSG_ERRQUEUE | libc::MSG_DONTWAIT) } >= 0 {
        let cm = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        if cm.is_null() { break; }
        #[repr(C)] struct SeErr { errno: u32, origin: u8, ty: u8, code: u8, pad: u8, info: u32, data: u32 }
        let se = unsafe { &*(libc::CMSG_DATA(cm) as *const SeErr) };
        let done = (se.data - se.info + 1) as usize;
        *pending = pending.saturating_sub(done);
        msg.msg_controllen = ctrl.len();
    }
}
fn reap_wait(fd: i32, pending: &mut usize, until: usize) {
    let mut spins = 0u64;
    while *pending > until {
        reap(fd, pending);
        if *pending > until { let mut pfd = libc::pollfd { fd, events: 0, revents: 0 }; unsafe { libc::poll(&mut pfd, 1, 100) }; spins += 1; if spins > 100_000 { panic!("brak potwierdzeń zerocopy"); } }
    }
}
