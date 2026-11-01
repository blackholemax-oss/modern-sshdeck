// 用真实 diff 文件验证 RTK 压缩率（手动跑：cargo run --release --example rtk_check）
fn main() {
    let path = std::env::args().nth(1).unwrap_or("tests/rtk_sample.diff".into());
    let raw = std::fs::read_to_string(&path).expect("read sample");
    let out = modern_sshdeck_lib::token::compress_tool_output(&raw);
    println!(
        "[rtk] {} bytes -> {} bytes, 节省 {:.1}%, filter={:?} applied={}",
        out.before_bytes,
        out.after_bytes,
        (out.before_bytes.saturating_sub(out.after_bytes)) as f64 / out.before_bytes as f64 * 100.0,
        out.filter,
        out.applied
    );
    if out.applied {
        let head: String = out.text.chars().take(400).collect();
        println!("--- 压缩后前 400 字符 ---\n{head}");
        println!("--- 尾部 ---\n{}", out.text.lines().rev().take(3).collect::<Vec<_>>().join("\n"));
    }
}
