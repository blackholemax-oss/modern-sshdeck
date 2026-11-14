//! IPC 命令层。
//!
//! 每个 `#[tauri::command]` 都是薄封装：解析参数 → 调子模块 → 返回 DTO。
//! 业务逻辑不写在这里，写在各 `ssh/` `forward/` `agent/` 模块内。

pub mod dto;
pub mod events;
pub mod extras;
pub mod fs;

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
    // 主机密钥策略 / known_hosts 路径是「连接期生效」的，改完立刻同步给会话管理器，
    // 否则要重启应用才生效（用户会以为设置没保存）。
    state.sessions.apply_host_key_settings(
        &updated.behavior.host_key_policy,
        &updated.behavior.known_hosts_path,
    );
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

/// 只握手不认证，取回服务端主机公钥（strict 策略下「未知主机」的指纹确认用）。
#[tauri::command]
pub async fn ssh_probe_host_key(
    state: State<'_, AppState>,
    host: String,
    port: u16,
) -> AppResult<HostKeyProbe> {
    state.sessions.probe_host_key(&host, port).await
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
pub async fn forward_stop(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    forward_id: String,
) -> AppResult<()> {
    state.forwards.stop(app, &forward_id).await
}

/// 查询单条规则（运行期最新状态 + 失败原因）。
#[tauri::command]
pub async fn forward_get(
    state: State<'_, AppState>,
    forward_id: String,
) -> AppResult<ForwardInfo> {
    state.forwards.get(&forward_id).await
}

/// 列举规则；传 `sessionId` 时只返回该会话名下的规则。
#[tauri::command]
pub async fn forward_list(
    state: State<'_, AppState>,
    session_id: Option<String>,
) -> AppResult<Vec<ForwardInfo>> {
    Ok(state
        .forwards
        .list(session_id.as_deref().map(str::trim).filter(|s| !s.is_empty()))
        .await)
}

/// 修改规则：原地重启并保留 `id`。
#[tauri::command]
pub async fn forward_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    forward_id: String,
    patch: ForwardPatch,
) -> AppResult<ForwardInfo> {
    state.forwards.update(app, &forward_id, patch).await
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

    // ⚠️ 必须用**用户真正选中的通道**（`active_model`）来列举模型，
    // 不能用遗留的 `llm.base_url` 字段。
    //
    // 真实缺陷：配置里 `activeModel = "ollama/qwen3:8b"`，而 `baseUrl` 仍是
    // 出厂默认的 `https://api.deepseek.com/v1` 且没有 Key。旧实现读 `baseUrl`，
    // 于是拿「用 Ollama」的意图去打 DeepSeek 的接口，上游回
    // `401 Authentication Fails (governor)` —— 用户看到「governor」无从判断
    // 真实原因其实是「请求打错了主机」。
    let channels: std::collections::HashMap<String, crate::llm::router::ChannelConfig> = cfg
        .llm
        .channels
        .iter()
        .map(|c| (c.provider_id.clone(), c.clone().into()))
        .collect();
    // 渠道 Key 只存持久层（public_config 只暴露 has_key）
    let store = crate::persist::store::PersistStore::new();
    let mut channels_with_key = channels;
    for (pid, k) in store.channel_api_keys() {
        if let Some(c) = channels_with_key.get_mut(&pid) {
            c.api_key = Some(k);
        }
    }
    let default_cfg = crate::llm::router::DefaultLlmConfig {
        base_url: cfg.llm.base_url.clone(),
        model: cfg.llm.model.clone(),
        api_key: state.persist.llm_api_key(),
    };
    let target = crate::llm::router::resolve(&cfg.llm.active_model, &channels_with_key, &default_cfg)?;

    let key = target.api_key.clone().unwrap_or_default();
    state
        .agent
        .list_models(&target.base_url, &key)
        .await
        .map_err(|e| {
            // 把「哪个通道、哪个主机」带进错误，避免下次又被误读
            AppError::Llm(format!(
                "[{}] {}",
                target.provider.name,
                e.message()
            ))
        })
}

#[tauri::command]
pub async fn agent_reset(state: State<'_, AppState>, run_id: String) -> AppResult<()> {
    state.agent.reset(&run_id).await
}
