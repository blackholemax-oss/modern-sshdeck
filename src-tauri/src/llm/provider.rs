//! LLM provider 抽象。

use async_trait::async_trait;

use crate::error::AppResult;
use crate::llm::types::{ChatMessage, ChatOutcome, ToolSpec};

#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// 流式对话；每收到增量调用 `on_delta`。
    async fn stream_chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        on_delta: &mut (dyn FnMut(crate::llm::types::Delta) + Send),
    ) -> AppResult<ChatOutcome>;

    /// 探测可用模型列表。
    async fn list_models(&self) -> AppResult<Vec<String>>;
}
