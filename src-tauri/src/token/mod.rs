//! 省 Token 模块：RTK 工具输出压缩（M-B1）与节省统计（M-B2）。
//!
//! 契约见 `docs/FEATURE-MODEL-PROVIDERS.md` §3.4 与 §4。
//!
//! 用法（运行时接入点，见契约 §6）：
//!
//! ```ignore
//! let outcome = token::compress_tool_output(&raw_tool_output);
//! token::GLOBAL.record(&outcome);
//! // 把 outcome.text 回填给模型
//! ```
//!
//! 本模块只做纯文本变换，不触网、不落盘、不依赖 Tauri。

pub mod rtk;
pub mod stats;

pub use rtk::{compress_tool_output, is_compressed, CompressOutcome, MARKER_PREFIX, MIN_BYTES};
pub use stats::{
    global_snapshot, CompressionAccumulator, CompressionStats, GLOBAL,
};

/// 压缩并把结果记入进程级统计，返回压缩后的文本。
///
/// 这是运行时（`AgentRuntime::send` 的 tool 结果分支）应该调用的单点入口。
pub fn compress_and_account(raw: &str) -> String {
    let outcome = compress_tool_output(raw);
    GLOBAL.record(&outcome);
    outcome.text
}

/// 清空全局压缩统计（供设置页「重置统计」）。
pub fn global_reset() {
    GLOBAL.reset();
}
