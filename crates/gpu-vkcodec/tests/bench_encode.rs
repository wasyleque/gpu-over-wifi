//! Mikropomiar (ręczny): ile kosztuje zakodowanie jednego polecenia o typowej postaci (rysowanie, ustawienie stanu, bariera).
//! `cargo test -p gpu-vkcodec --release --test bench_encode -- --ignored --nocapture`

use gpu_proto::meta::find_command;
use gpu_vkcodec::codec::{encode_command, Identity};
use gpu_wire::Encoder;

fn bench(name: &str, args: &[u64]) {
    let (op, c) = find_command(name).unwrap_or_else(|| panic!("brak {name}"));
    assert_eq!(args.len(), c.params().len(), "{name}: liczba parametrów");
    let n = 300_000;
    let t = std::time::Instant::now();
    let mut total = 0usize;
    for _ in 0..n {
        let mut e = Encoder::new();
        unsafe { encode_command(op, args.as_ptr() as *const u8, &mut e, &Identity) }.unwrap();
        total += e.finish().len();
    }
    println!("ENCODE {name}: {:.0} ns na polecenie, {} B", t.elapsed().as_nanos() as f64 / n as f64, total / n);
}

#[test]
#[ignore = "mikropomiar"]
fn encode_cost_of_typical_commands() {
    bench("vkCmdDraw", &[1, 3, 1, 0, 0]);
    bench("vkCmdDrawIndexed", &[1, 36, 1, 0, 0, 0]);
    bench("vkCmdDispatch", &[1, 8, 8, 1]);
    bench("vkCmdBindPipeline", &[1, 0, 77]);
    bench("vkCmdSetViewportWithCount", &[1, 0, 0]);
    bench("vkCmdPushConstants", &[1, 5, 0x20, 0, 0, 0]);
    bench("vkResetCommandPool", &[1, 2, 0]);
}
