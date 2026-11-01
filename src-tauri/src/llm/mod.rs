//! LLM provider 抽象与 OpenAI 兼容实现。
//!
//! 模块划分见 `docs/FEATURE-MODEL-PROVIDERS.md` §2：
//! - [`registry`] M-A1 提供商注册表
//! - [`channels`] M-A2 通道发现与探测
//! - [`router`] M-A3 `provider/model` 解析与降级链
pub mod channels;
pub mod openai;
pub mod provider;
pub mod registry;
pub mod router;
pub mod types;
