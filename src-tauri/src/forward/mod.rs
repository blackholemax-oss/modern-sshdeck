//! 端口转发。
//!
//! - `local`：`-L` 语义（本机监听 → SSH `direct-tcpip`）
//! - `remote`：`-R` 语义（服务端监听 → `forwarded-tcpip` → 本机目标）
//! - `registry`：转发规则的增删改查与状态广播
//! - `supervisor`：规则持久化 + 断线 / 重启自动恢复
pub mod local;
pub mod registry;
pub mod remote;
pub mod supervisor;
