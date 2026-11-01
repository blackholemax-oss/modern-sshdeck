//! 新增能力的 IPC 命令：模型渠道、省 Token 统计、审计日志。
//!
//! 独立成文件，避免把已验证稳定的既有 `ipc/mod.rs` 搅乱。
//! 命令需在 `lib.rs` 的 `generate_handler!` 里注册。

use tauri::{Emitter, State};

use crate::error::{AppError, AppResult};
use crate::ipc::dto::{AuditEntryDto, CompressionStatsDto, ProbeResultDto, ProviderDto};
use crate::state::AppState;

// ───────────────────────────── 模型渠道 ─────────────────────────────

/// 列出内置提供商注册表，并合入用户已配置的渠道状态。
///
/// `has_key` / `enabled` / `base_url` 来自配置镜像；注册表本身是编译期常量，
/// 只有配置是会变的，所以两者合并后再给前端才有意义。
#[tauri::command]
pub async fn llm_providers(state: State<'_, AppState>) -> AppResult<Vec<ProviderDto>> {
    let cfg = state.config.lock().await.clone();

    let mut out = Vec::new();
    for p in crate::llm::registry::all() {
        let channel = cfg.llm.channels.iter().find(|c| c.provider_id == p.id);

        out.push(ProviderDto {
            id: p.id.to_string(),
            name: p.name.to_string(),
            category: p.category.as_str().to_string(),
            base_url: channel
                .and_then(|c| c.base_url_override.clone())
                .or_else(|| p.base_url.map(str::to_string)),
            docs_url: p.docs_url.map(str::to_string),
            note: p.note.map(str::to_string),
            requires_key: !matches!(p.auth, crate::llm::registry::AuthKind::None),
            has_key: channel
                .and_then(|c| c.api_key.clone())
                .map(|k| !k.trim().is_empty())
                .unwrap_or(false),
            enabled: channel.map(|c| c.enabled).unwrap_or(true),
        });
    }
    Ok(out)
}

/// 探测单个渠道：区分「连不上 / 无模型 / Key 无效 / 仅目录 / 可用」。
#[tauri::command]
pub async fn llm_probe_channel(
    state: State<'_, AppState>,
    provider_id: String,
    base_url_override: Option<String>,
) -> AppResult<ProbeResultDto> {
    let entry = crate::llm::registry::find(&provider_id)
        .ok_or_else(|| AppError::InvalidInput(format!("未知提供商：{provider_id}")))?;

    let cfg = state.config.lock().await.clone();
    let channel = cfg.llm.channels.iter().find(|c| c.provider_id == provider_id);

    // Key 来源：渠道 > 老配置的全局 key（仅对 builtin 生效）
    let key = channel
        .and_then(|c| c.api_key.clone())
        .filter(|k| !k.trim().is_empty())
        .or_else(|| {
            if provider_id == "builtin" {
                let k = state.persist.llm_api_key();
                (!k.trim().is_empty()).then_some(k)
            } else {
                None
            }
        });

    let override_url = base_url_override
        .filter(|s| !s.trim().is_empty())
        .or_else(|| channel.and_then(|c| c.base_url_override.clone()));

    let outcome =
        crate::llm::channels::probe(entry, key.as_deref(), override_url.as_deref()).await;

    Ok(ProbeResultDto {
        provider_id,
        status: outcome.status.as_str().to_string(),
        message: outcome.message,
        models: outcome.models,
        free_model_count: outcome.free_model_count,
    })
}

/// 发现某渠道的模型清单（探测的轻量版，只返回 id）。
#[tauri::command]
pub async fn llm_discover_models(
    state: State<'_, AppState>,
    provider_id: String,
    base_url_override: Option<String>,
) -> AppResult<Vec<String>> {
    let entry = crate::llm::registry::find(&provider_id)
        .ok_or_else(|| AppError::InvalidInput(format!("未知提供商：{provider_id}")))?;

    let cfg = state.config.lock().await.clone();
    let channel = cfg.llm.channels.iter().find(|c| c.provider_id == provider_id);
    let key = channel
        .and_then(|c| c.api_key.clone())
        .filter(|k| !k.trim().is_empty());

    let override_url = base_url_override
        .filter(|s| !s.trim().is_empty())
        .or_else(|| channel.and_then(|c| c.base_url_override.clone()));

    crate::llm::channels::discover_models(entry, key.as_deref(), override_url.as_deref()).await
}

// ───────────────────────────── 省 Token ─────────────────────────────

/// 压缩统计快照（压缩前/后字节、节省比例、块数、调用数）。
#[tauri::command]
pub async fn token_stats() -> AppResult<CompressionStatsDto> {
    let s = crate::token::global_snapshot();
    Ok(CompressionStatsDto {
        before_bytes: s.before_bytes,
        after_bytes: s.after_bytes,
        saved_bytes: s.saved_bytes,
        blocks_compressed: s.blocks_compressed,
        calls: s.calls,
        saved_ratio: s.saved_ratio(),
    })
}

/// 重置压缩统计。
#[tauri::command]
pub async fn token_stats_reset() -> AppResult<()> {
    crate::token::global_reset();
    Ok(())
}

// ───────────────────────────── 审计 ─────────────────────────────

/// 读取最近的审批审计记录（默认 50 条，上限 500）。
#[tauri::command]
pub async fn audit_tail(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> AppResult<Vec<AuditEntryDto>> {
    let n = limit.unwrap_or(50).min(500);
    Ok(state
        .agent
        .audit()
        .tail(n)
        .into_iter()
        .map(|e| AuditEntryDto {
            ts: e.ts,
            session_id: e.session_id,
            command: e.command,
            risk: e.risk,
            decision: e.decision,
            reason: e.reason,
        })
        .collect())
}

/// 审计总条数。
#[tauri::command]
pub async fn audit_count(state: State<'_, AppState>) -> AppResult<usize> {
    Ok(state.agent.audit().count())
}
