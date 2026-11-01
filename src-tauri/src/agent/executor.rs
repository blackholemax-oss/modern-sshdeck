//! 工具执行。
//!
//! **所有权**：Backend-C 完整实现。

use std::sync::Arc;

use crate::error::{AppError, AppResult};
use crate::ssh::manager::SessionManager;

pub struct ToolExecutor {
    pub sessions: Arc<SessionManager>,
}

impl ToolExecutor {
    pub fn new(sessions: Arc<SessionManager>) -> Self {
        Self { sessions }
    }

    pub async fn run_command(&self, session_id: &str, command: &str) -> AppResult<String> {
        self.sessions
            .run_command(session_id, command, std::time::Duration::from_secs(30))
            .await
            .map_err(|e| AppError::Agent(e.message()))
    }
}
