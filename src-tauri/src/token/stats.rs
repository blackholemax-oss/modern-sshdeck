//! 省 Token 统计（契约 §3.4）。
//!
//! 线程安全累计器：每次工具输出过 RTK 都记一条，字段与 `CompressionStats` 对齐。
//! 全部用 `AtomicU64`，`record` 无锁、可从任意线程调用。

use std::sync::atomic::{AtomicU64, Ordering};

use super::rtk::CompressOutcome;

/// 省 Token 累计快照（契约 §3.4）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompressionStats {
    /// 压缩前总字节数。
    pub before_bytes: u64,
    /// 压缩后总字节数。
    pub after_bytes: u64,
    /// 节省的字节数（`before_bytes - after_bytes`）。
    pub saved_bytes: u64,
    /// 真正发生压缩的块数。
    pub blocks_compressed: u64,
    /// 调用总次数（含放行）。
    pub calls: u64,
}

impl CompressionStats {
    /// 节省比例，0.0 ~ 1.0。
    pub fn saved_ratio(&self) -> f64 {
        if self.before_bytes == 0 {
            0.0
        } else {
            self.saved_bytes as f64 / self.before_bytes as f64
        }
    }

    /// 百分比文本，如 `"82.4%"`。
    pub fn saved_percent(&self) -> String {
        format!("{:.1}%", self.saved_ratio() * 100.0)
    }
}

/// 线程安全累计器。
#[derive(Debug, Default)]
pub struct CompressionAccumulator {
    before_bytes: AtomicU64,
    after_bytes: AtomicU64,
    saved_bytes: AtomicU64,
    blocks_compressed: AtomicU64,
    calls: AtomicU64,
}

impl CompressionAccumulator {
    /// 常量构造，可用于 `static`。
    pub const fn new() -> Self {
        Self {
            before_bytes: AtomicU64::new(0),
            after_bytes: AtomicU64::new(0),
            saved_bytes: AtomicU64::new(0),
            blocks_compressed: AtomicU64::new(0),
            calls: AtomicU64::new(0),
        }
    }

    /// 记录一次压缩结果。`applied == false` 也计数（`calls` 与 `before_bytes` 都要准确）。
    pub fn record(&self, outcome: &CompressOutcome) {
        let before = outcome.before_bytes;
        let after = outcome.after_bytes;
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.before_bytes.fetch_add(before, Ordering::Relaxed);
        self.after_bytes.fetch_add(after, Ordering::Relaxed);
        self.saved_bytes
            .fetch_add(before.saturating_sub(after), Ordering::Relaxed);
        if outcome.applied {
            self.blocks_compressed.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 读取当前快照。
    pub fn snapshot(&self) -> CompressionStats {
        CompressionStats {
            before_bytes: self.before_bytes.load(Ordering::Relaxed),
            after_bytes: self.after_bytes.load(Ordering::Relaxed),
            saved_bytes: self.saved_bytes.load(Ordering::Relaxed),
            blocks_compressed: self.blocks_compressed.load(Ordering::Relaxed),
            calls: self.calls.load(Ordering::Relaxed),
        }
    }

    /// 归零。
    pub fn reset(&self) {
        self.before_bytes.store(0, Ordering::Relaxed);
        self.after_bytes.store(0, Ordering::Relaxed);
        self.saved_bytes.store(0, Ordering::Relaxed);
        self.blocks_compressed.store(0, Ordering::Relaxed);
        self.calls.store(0, Ordering::Relaxed);
    }
}

/// 进程级累计器，供运行时/UI 直接引用。
pub static GLOBAL: CompressionAccumulator = CompressionAccumulator::new();

/// 进程级快照的快捷方式。
pub fn global_snapshot() -> CompressionStats {
    GLOBAL.snapshot()
}

#[cfg(test)]
mod tests {
    use super::super::rtk::compress_tool_output;
    use super::*;

    fn outcome(before: u64, after: u64, applied: bool) -> CompressOutcome {
        CompressOutcome {
            text: String::new(),
            before_bytes: before,
            after_bytes: after,
            applied,
            filter: if applied { Some("unified_diff") } else { None },
        }
    }

    #[test]
    fn stats_accumulate_correctly() {
        let acc = CompressionAccumulator::new();
        // 一次真压缩：10000 → 1200
        acc.record(&outcome(10_000, 1_200, true));
        // 一次放行：900 → 900，不计入 blocks_compressed
        acc.record(&outcome(900, 900, false));
        // 再一次压缩：5000 → 800
        acc.record(&outcome(5_000, 800, true));

        let s = acc.snapshot();
        assert_eq!(s.calls, 3, "每次调用都要计数");
        assert_eq!(s.blocks_compressed, 2, "只有 applied 计入块数");
        assert_eq!(s.before_bytes, 15_900);
        assert_eq!(s.after_bytes, 2_900);
        assert_eq!(s.saved_bytes, 13_000);
        assert_eq!(s.saved_bytes, s.before_bytes - s.after_bytes);
        assert!(s.saved_ratio() > 0.81 && s.saved_ratio() < 0.82);
        assert_eq!(s.saved_percent(), "81.8%");

        acc.reset();
        let s = acc.snapshot();
        assert_eq!(s, CompressionStats::default(), "reset 后应回到全零");
    }

    #[test]
    fn stats_record_real_outcome_and_zero_ratio_is_safe() {
        let acc = CompressionAccumulator::new();
        assert_eq!(acc.snapshot().saved_ratio(), 0.0, "空统计不能除以 0");

        let raw = "error: cannot open /etc/nope.conf\n";
        let out = compress_tool_output(raw);
        acc.record(&out);
        let s = acc.snapshot();
        assert_eq!(s.calls, 1);
        assert_eq!(s.blocks_compressed, 0);
        assert_eq!(s.before_bytes, raw.len() as u64);
        assert_eq!(s.saved_bytes, 0);
    }
}
