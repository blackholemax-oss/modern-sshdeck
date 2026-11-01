//! SSH 会话管理。
//!
//! 对外暴露的最小接口（Backend-A 实现，其它模块只经由这里访问会话）：
//! - `create` / `close` / `list` / `get`
//! - `write_input` / `resize`
//! - `run_command`（Agent 工具调用用：在会话里执行一次性命令并收集输出）

pub mod auth;
pub mod config;
pub mod manager;
pub mod session;
