//! 统一错误类型。
//!
//! 所有 Tauri command 返回 `Result<T, AppError>`；`AppError` 序列化为
//! `{ code, message }`，与前端 `src/shared/ipc.ts` 的 `IpcError` 对应。

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("ssh error: {0}")]
    Ssh(String),

    #[error("forward error: {0}")]
    Forward(String),

    #[error("llm error: {0}")]
    Llm(String),

    #[error("agent error: {0}")]
    Agent(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("io error: {0}")]
    Io(String),

    #[error("persist error: {0}")]
    Persist(String),
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Ssh(_) => "SSH_ERROR",
            AppError::Forward(_) => "FORWARD_ERROR",
            AppError::Llm(_) => "LLM_ERROR",
            AppError::Agent(_) => "AGENT_ERROR",
            AppError::NotFound(_) => "NOT_FOUND",
            AppError::InvalidInput(_) => "INVALID_INPUT",
            AppError::Io(_) => "IO_ERROR",
            AppError::Persist(_) => "PERSIST_ERROR",
        }
    }

    pub fn message(&self) -> String {
        match self {
            AppError::Ssh(m)
            | AppError::Forward(m)
            | AppError::Llm(m)
            | AppError::Agent(m)
            | AppError::NotFound(m)
            | AppError::InvalidInput(m)
            | AppError::Io(m)
            | AppError::Persist(m) => m.clone(),
        }
    }
}

/// 序列化形状：`{ "code": "...", "message": "..." }`
#[derive(Debug, Serialize)]
pub struct IpcError {
    pub code: String,
    pub message: String,
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        IpcError {
            code: self.code().to_string(),
            message: self.message(),
        }
        .serialize(serializer)
    }
}

impl From<std::io::Error> for AppError {
    fn from(value: std::io::Error) -> Self {
        AppError::Io(value.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(value: serde_json::Error) -> Self {
        AppError::Persist(value.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;
