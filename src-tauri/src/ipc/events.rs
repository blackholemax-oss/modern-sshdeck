//! 应用内所有事件名与 payload 结构。
//!
//! 事件名必须与 `src/shared/ipc.ts` 的 `Events` 常量一致。

use serde::Serialize;

use crate::ipc::dto::{ForwardState, Risk, SessionState};

pub const TERMINAL_DATA: &str = "terminal:data";
pub const TERMINAL_CLOSED: &str = "terminal:closed";
pub const SESSION_STATE: &str = "session:state";
pub const FORWARD_STATE: &str = "forward:state";
pub const AGENT_DELTA: &str = "agent:delta";
pub const AGENT_TOOL_CALL: &str = "agent:tool_call";
pub const AGENT_TOOL_RESULT: &str = "agent:tool_result";
pub const AGENT_APPROVAL_REQUIRED: &str = "agent:approval_required";
pub const AGENT_DONE: &str = "agent:done";
pub const AGENT_ERROR: &str = "agent:error";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalDataEvent {
    pub session_id: String,
    /// base64 编码的输出块
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalClosedEvent {
    pub session_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStateEvent {
    pub session_id: String,
    pub state: SessionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardStateEvent {
    pub forward_id: String,
    pub state: ForwardState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDeltaEvent {
    pub run_id: String,
    pub delta: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolCallEvent {
    pub run_id: String,
    pub call_id: String,
    pub name: String,
    pub args: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolResultEvent {
    pub run_id: String,
    pub call_id: String,
    pub ok: bool,
    pub output: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentApprovalRequiredEvent {
    pub run_id: String,
    pub approval_id: String,
    pub command: String,
    pub risk: Risk,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDoneEvent {
    pub run_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentErrorEvent {
    pub run_id: String,
    pub code: String,
    pub message: String,
}
