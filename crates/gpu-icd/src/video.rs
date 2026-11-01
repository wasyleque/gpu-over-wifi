//! Odbiornik wideo: osobne połączenie z demonem, wątek odbierający ramki obrazu i wyświetlający je lokalnie.
//! Aplikacja nie czeka na odczyt klatki: `Present` tylko zleca serwerowi wysłanie, a ten wypycha ramki tutaj.

use std::net::{Shutdown, TcpStream};
use std::thread::JoinHandle;

use gpu_proto::msg::{Request, Response};
use gpu_proto::video::{decompress_lz4_into, raw_pixels, Codec, PixelFormat, VideoHeader, HEADER_LEN};
use gpu_renderd::client::Client;

use crate::present::{rgba_to_bgra, Presenter};
use crate::with_local;

pub struct VideoThread {
    shutdown: TcpStream,
    handle: Option<JoinHandle<VideoStats>>,
}

/// Liczniki odbiornika (zwracane przy zatrzymaniu).
#[derive(Debug, Default, Clone, Copy)]
pub struct VideoStats {
    pub frames: u64,
    pub bytes: u64,
    pub recv_ns: u64,
    pub decode_ns: u64,
    pub present_ns: u64,
}

impl VideoThread {
    /// Łączy się z demonem, podłącza do sesji `session` i uruchamia wątek wyświetlający ramki na `surface`.
    pub fn start(addr: &str, token: &[u8], session: u64, surface: u64, width: u32, height: u32) -> Result<VideoThread, String> {
        let mut c = Client::connect(addr, token).map_err(|e| format!("połączenie wideo: {e:?}"))?;
        match c.call(&Request::AttachVideo { session }) {
            Ok(Response::U32(0)) => {}
            other => return Err(format!("AttachVideo: {other:?}")),
        }
        c.set_read_timeout(None).map_err(|e| format!("{e:?}"))?;
        let shutdown = c.shutdown_handle().map_err(|e| format!("{e:?}"))?;
        let handle = std::thread::Builder::new().name("gpu-icd-video".into()).spawn(move || run(c, surface, width, height)).map_err(|e| e.to_string())?;
        Ok(VideoThread { shutdown, handle: Some(handle) })
    }

    /// Zamyka połączenie i czeka na zakończenie wątku (który niszczy lokalny prezenter).
    pub fn stop(&mut self) -> VideoStats {
        let _ = self.shutdown.shutdown(Shutdown::Both);
        self.handle.take().and_then(|h| h.join().ok()).unwrap_or_default()
    }
}

impl Drop for VideoThread {
    fn drop(&mut self) {
        if self.handle.is_some() {
            self.stop();
        }
    }
}

fn run(mut c: Client, surface: u64, width: u32, height: u32) -> VideoStats {
    let mut st = VideoStats::default();
    let mut presenter: Option<Presenter> = None;
    let mut bgra: Vec<u8> = Vec::new();
    let mut decoded: Vec<u8> = Vec::new();
    loop {
        let t0 = std::time::Instant::now();
        let Ok(frame) = c.recv_frame() else { break };
        st.recv_ns += t0.elapsed().as_nanos() as u64;
        let Some(head) = frame.payload.get(..HEADER_LEN) else { continue };
        let (Ok(h), Some(body)) = (VideoHeader::decode(head), frame.payload.get(HEADER_LEN..)) else { continue };
        if h.width as u32 != width || h.height as u32 != height {
            continue; // zmiana rozmiaru wymaga nowego swapchaina
        }
        let t_dec = std::time::Instant::now();
        let px: &[u8] = match h.codec {
            Codec::Raw => match raw_pixels(&h, body) {
                Ok(p) => p,
                Err(_) => continue,
            },
            Codec::Lz4 => {
                decoded.resize(width as usize * height as usize * 4, 0);
                if decompress_lz4_into(body, &mut decoded).is_err() {
                    continue;
                }
                &decoded
            }
            Codec::Hevc | Codec::H264 => continue, // M5: dekodery wideo dojdą z VA-API
        };
        st.decode_ns += t_dec.elapsed().as_nanos() as u64;
        let frame_bgra: &[u8] = if h.format == PixelFormat::Rgba8 {
            bgra.resize(px.len(), 0);
            if !rgba_to_bgra(px, &mut bgra) {
                continue;
            }
            &bgra
        } else {
            px
        };
        let t1 = std::time::Instant::now();
        if presenter.is_none() {
            presenter = with_local(|l| Presenter::new(l, surface, width, height)).and_then(|r| match r {
                Ok(p) => Some(p),
                Err(e) => {
                    eprintln!("gpu-icd: lokalny prezenter: {e}");
                    None
                }
            });
        }
        if let Some(p) = presenter.as_mut() {
            let _ = with_local(|l| p.present(l, frame_bgra));
        }
        st.present_ns += t1.elapsed().as_nanos() as u64;
        st.frames += 1;
        st.bytes += frame.payload.len() as u64;
        if let (Ok(path), 1) = (std::env::var("GPU_ICD_DUMP_FRAME"), st.frames) {
            let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
            for p in frame_bgra.chunks(4) {
                ppm.extend([p[2], p[1], p[0]]);
            }
            let _ = std::fs::write(path, ppm);
        }
    }
    if let Some(mut p) = presenter {
        with_local(|l| p.destroy(l));
    }
    if std::env::var("GPU_ICD_STATS").is_ok() && st.frames > 0 {
        let per = |v: u64| v as f64 / st.frames as f64 / 1e6;
        eprintln!("gpu-icd wideo: klatek {}, {:.2} MB ({:.0} KB/klatkę), na klatkę: odbiór {:.1} ms, dekodowanie {:.1} ms, wyświetlenie {:.1} ms", st.frames, st.bytes as f64 / 1e6, st.bytes as f64 / st.frames as f64 / 1e3, per(st.recv_ns), per(st.decode_ns), per(st.present_ns));
    }
    st
}
