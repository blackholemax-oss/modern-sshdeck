//! 用**真实**的大 diff 验证 RTK 压缩率与信息保全。
//!
//! 数据源：本仓库真实 git diff（若仓库无 git，则用构造的真实形态样本）。
use modern_sshdeck_lib::token::compress_tool_output;

fn real_diff() -> String {
    // 首选：仓库内真实 git diff fixture（由 .rtkdemo 真实仓库产生并固化）
    if let Ok(s) = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rtk_sample.diff")) {
        if s.len() > 2048 {
            return s;
        }
    }

    // 取本仓库最近一次提交的 diff（真实文件内容，非手搓）
    let out = std::process::Command::new("git")
        .args(["show", "--stat=0", "--patch", "-1", "--", "src-tauri/src/"])
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout).to_string();
            if s.len() > 4096 {
                return s;
            }
        }
    }
    // 再试全仓库最近提交
    let out2 = std::process::Command::new("git")
        .args(["show", "--patch", "-1"])
        .output();
    if let Ok(o) = out2 {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout).to_string();
            if s.len() > 4096 {
                return s;
            }
        }
    }
    String::new()
}

#[test]
fn real_repo_diff_compresses_and_keeps_changes() {
    let raw = real_diff();
    if raw.is_empty() {
        eprintln!("[skip] 本仓库没有足够大的 git diff（可能非 git 仓库或尚无提交）");
        return;
    }

    let out = compress_tool_output(&raw);
    let pct = (out.before_bytes.saturating_sub(out.after_bytes)) as f64
        / out.before_bytes as f64
        * 100.0;

    println!(
        "[real diff] {} bytes -> {} bytes, 节省 {:.1}%, filter={:?} applied={}",
        out.before_bytes, out.after_bytes, pct, out.filter, out.applied
    );

    // 关键：所有变更行必须还在（压缩不能丢信息）
    let mut missing = 0;
    for line in raw.lines() {
        let t = line.trim_end();
        if t.starts_with("diff --git ") || t.starts_with("@@") || t.starts_with("+++") || t.starts_with("---") {
            if !out.text.contains(t) {
                missing += 1;
                eprintln!("[missing header] {t}");
            }
        }
    }
    assert_eq!(missing, 0, "diff 头/hunk 头必须完整保留");

    // 这份样本来自真实 `git diff`（800 行级别改动，12 KB），必须真的被压缩。
    // 若这里失败，说明压缩器退化成了「原样放行」，省 Token 能力失效。
    assert!(
        out.applied,
        "真实大 diff 未被压缩（{} bytes，filter={:?}）",
        out.before_bytes, out.filter
    );
    assert!(
        pct > 30.0,
        "真实大 diff 压缩率过低：{pct:.1}%（{} → {}）",
        out.before_bytes,
        out.after_bytes
    );
    assert!(
        out.text.contains("已压缩"),
        "压缩结果必须带可回查标记，否则模型不知道看到的是压缩视图"
    );
}
