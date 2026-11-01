//! IPC 命令层。
//!
//! 每个 `#[tauri::command]` 都是薄封装：解析参数 → 调子模块 → 返回 DTO。
//! 业务逻辑不写在这里，写在各 `ssh/` `forward/` `agent/` 模块内。

pub mod dto;
pub mod events;
pub mod extras;

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::ipc::dto::*;
use crate::state::AppState;

// ───────────────────────────── 配置 ─────────────────────────────

#[tauri::command]
pub async fn app_get_config(state: State<'_, AppState>) -> AppResult<AppConfig> {
    Ok(state.config.lock().await.clone())
}

#[tauri::command]
pub async fn app_set_config(
    state: State<'_, AppState>,
    config: AppConfigPatch,
) -> AppResult<AppConfig> {
    let updated = state.persist.apply_patch(config)?;
    *state.config.lock().await = updated.clone();
    Ok(updated)
}

// ───────────────────────────── 主机 ─────────────────────────────

#[tauri::command]
pub async fn hosts_list(state: State<'_, AppState>) -> AppResult<Vec<HostProfile>> {
    Ok(state.persist.list_hosts())
}

#[tauri::command]
pub async fn hosts_upsert(
    state: State<'_, AppState>,
    host: HostProfile,
) -> AppResult<HostProfile> {
    state.persist.upsert_host(host)
}

#[tauri::command]
pub async fn hosts_delete(state: State<'_, AppState>, id: String) -> AppResult<()> {
    state.persist.delete_host(&id)
}

// ───────────────────────────── SSH ─────────────────────────────

#[tauri::command]
pub async fn ssh_connect(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    req: ConnectRequest,
) -> AppResult<SessionInfo> {
    if req.host.trim().is_empty() {
        return Err(AppError::InvalidInput("host is required".into()));
    }
    if req.username.trim().is_empty() {
        return Err(AppError::InvalidInput("username is required".into()));
    }
    state.sessions.create(app, req).await
}

#[tauri::command]
pub async fn ssh_disconnect(state: State<'_, AppState>, session_id: String) -> AppResult<()> {
    state.sessions.close(&session_id).await
}

#[tauri::command]
pub async fn ssh_list_sessions(state: State<'_, AppState>) -> AppResult<Vec<SessionInfo>> {
    Ok(state.sessions.list().await)
}

#[tauri::command]
pub async fn terminal_input(
    state: State<'_, AppState>,
    session_id: String,
    data: String,
) -> AppResult<()> {
    state.sessions.write_input(&session_id, data.as_bytes()).await
}

#[tauri::command]
pub async fn terminal_resize(
    state: State<'_, AppState>,
    session_id: String,
    cols: u32,
    rows: u32,
) -> AppResult<()> {
    state.sessions.resize(&session_id, cols, rows).await
}

// ───────────────────────────── 端口转发 ─────────────────────────────

#[tauri::command]
pub async fn forward_start(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    req: ForwardRequest,
) -> AppResult<ForwardInfo> {
    state.forwards.start(app, req).await
}

#[tauri::command]
pub async fn forward_stop(state: State<'_, AppState>, forward_id: String) -> AppResult<()> {
    state.forwards.stop(&forward_id).await
}

#[tauri::command]
pub async fn forward_list(state: State<'_, AppState>) -> AppResult<Vec<ForwardInfo>> {
    Ok(state.forwards.list().await)
}

// ───────────────────────────── Agent ─────────────────────────────

#[tauri::command]
pub async fn agent_send(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    req: AgentSendRequest,
) -> AppResult<()> {
    if req.message.trim().is_empty() {
        return Err(AppError::InvalidInput("message is required".into()));
    }
    state
        .agent
        .send(app, req, state.persist.llm_api_key())
        .await
}

#[tauri::command]
pub async fn agent_abort(state: State<'_, AppState>, run_id: String) -> AppResult<()> {
    state.agent.abort(&run_id).await
}

#[tauri::command]
pub async fn agent_approve(
    state: State<'_, AppState>,
    req: AgentApproveRequest,
) -> AppResult<()> {
    // 高危（Dangerous）命令必须携带精确短语「确认执行」，否则运行时按拒绝处理。
    state
        .agent
        .approve_with_phrase(&req.approval_id, req.approved, req.confirm_phrase.as_deref())
        .await
}

#[tauri::command]
pub async fn agent_list_models(state: State<'_, AppState>) -> AppResult<Vec<AgentModelInfo>> {
    let cfg = state.config.lock().await.clone();
    let key = state.persist.llm_api_key();
    state.agent.list_models(&cfg.llm.base_url, &key).await
}

#[tauri::command]
pub async fn agent_reset(state: State<'_, AppState>, run_id: String) -> AppResult<()> {
    state.agent.reset(&run_id).await
}
