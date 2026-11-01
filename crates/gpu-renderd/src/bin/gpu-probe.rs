//! `gpu-probe`: łączy się z `gpu-renderd` i wypisuje zdalne karty Vulkana.
//! Użycie: gpu-probe <adres:port> --token-file <plik>

use gpu_proto::msg::{PdFunc, Request, Response};
use gpu_renderd::client::{Client, ClientError};
use gpu_renderd::handler::decode_extensions;

fn u32_at(b: &[u8], off: usize) -> u32 {
    b.get(off..off + 4).and_then(|s| s.try_into().ok()).map(u32::from_le_bytes).unwrap_or(0)
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    b.get(off..off + 8).and_then(|s| s.try_into().ok()).map(u64::from_le_bytes).unwrap_or(0)
}

fn ver(v: u32) -> String {
    format!("{}.{}.{}", v >> 22, (v >> 12) & 0x3ff, v & 0xfff)
}

fn run(addr: &str, token: &[u8]) -> Result<(), ClientError> {
    let t0 = std::time::Instant::now();
    let mut c = Client::connect(addr, token)?;
    println!("połączono z {addr} w {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
    let Response::U32(v) = c.call(&Request::EnumInstanceVersion)? else { return Ok(()) };
    println!("wersja instancji Vulkan: {}", ver(v));
    if let Response::Data(d) = c.call(&Request::EnumInstanceExtensions)? {
        println!("rozszerzenia instancji: {}", decode_extensions(&d).map(|e| e.len()).unwrap_or(0));
    }
    let Response::Handle(inst) = c.call(&Request::CreateInstance { api_version: v, extensions: vec![] })? else { return Ok(()) };
    let Response::Handles(pds) = c.call(&Request::EnumPhysicalDevices { instance: inst })? else { return Ok(()) };
    println!("urządzeń fizycznych: {}", pds.len());
    for (i, p) in pds.iter().enumerate() {
        let t = std::time::Instant::now();
        let Response::Data(d) = c.call(&Request::GetPhysicalDevice { pdev: *p, func: PdFunc::Properties2, arg: 0, count: 0, chain: vec![] })? else { continue };
        let rtt = t.elapsed().as_secs_f64() * 1000.0;
        // VkPhysicalDeviceProperties2: properties w offsecie 16: apiVersion, driverVersion, vendorID, deviceID, deviceType, deviceName[256].
        let name = d.get(36..36 + 256).map(|n| n.split(|&b| b == 0).next().unwrap_or(&[]).to_vec()).unwrap_or_default();
        println!(
            "[{i}] {}  api {}  vendor {:#06x}  device {:#06x}  typ {}  (zapytanie {rtt:.2} ms)",
            String::from_utf8_lossy(&name),
            ver(u32_at(&d, 16)),
            u32_at(&d, 24),
            u32_at(&d, 28),
            u32_at(&d, 32)
        );
        if let Response::Data(m) = c.call(&Request::GetPhysicalDevice { pdev: *p, func: PdFunc::MemoryProperties2, arg: 0, count: 0, chain: vec![] })? {
            // VkPhysicalDeviceMemoryProperties: memoryTypeCount@16, memoryTypes[32]*8 @20, memoryHeapCount@276, heaps (size u64, flags u32) @280, po 16 B.
            let heaps = u32_at(&m, 276) as usize;
            let list: Vec<String> = (0..heaps.min(16)).map(|h| format!("{:.1} GiB", u64_at(&m, 280 + h * 16) as f64 / (1u64 << 30) as f64)).collect();
            println!("    stosy pamięci: {}", list.join(", "));
        }
        if let Response::Data(e) = c.call(&Request::EnumDeviceExtensions { pdev: *p })? {
            println!("    rozszerzenia urządzenia: {}", decode_extensions(&e).map(|e| e.len()).unwrap_or(0));
        }
    }
    c.call(&Request::DestroyInstance { instance: inst })?;
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(addr), Some(pos)) = (args.first(), args.iter().position(|a| a == "--token-file")) else {
        eprintln!("Użycie: gpu-probe <adres:port> --token-file <plik>");
        std::process::exit(2);
    };
    let Some(file) = args.get(pos + 1) else { std::process::exit(2) };
    let token = std::fs::read_to_string(file).unwrap_or_else(|e| {
        eprintln!("Nie można odczytać {file}: {e}");
        std::process::exit(1);
    });
    if let Err(e) = run(addr, token.trim().as_bytes()) {
        eprintln!("Błąd: {e:?}");
        std::process::exit(1);
    }
}
