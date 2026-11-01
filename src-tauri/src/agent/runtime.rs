//! Agent 运行时。
//!
//! **所有权**：Backend-C 完整实现。
//!
//! 契约：
//! - `send`：发起一轮对话（含 tool loop）——**立即返回**，循环跑在后台 task
//! - `abort`：中断当前 run（每轮/每次等待都检查取消标志）
//! - `approve`：放行/拒绝一个待审批工具调用（oneshot 派发）
//! - `approve_with_phrase`：放行/拒绝 + 高危命令的二次确认短语（供 `ipc::agent_approve`）
//! - `reset`：清空该 run 的消息历史
//! - `list_models`：转发到 provider
//!
//! 审批分级（契约 `docs/FEATURE-MODEL-PROVIDERS.md` §5）：
//! - `ReadOnly` → 自动执行，审计记 `auto`
//! - `Mutating` → 单次审批，通过记 `approved`，拒绝 `rejected`，超时 `timeout`
//! - `Dangerous` → 单次审批 **+** 二次确认短语（`确认执行`），短语不精确匹配一律 `rejected`
//!
//! 每一次审批（含自动放行）都落一条审计到 `audit.jsonl`（见 [`crate::agent::audit`]）。
//!
//! 并发模型：
//! - `history`：`run_id -> Vec<ChatMessage>`；运行开始时把消息"取走"到本地，
//!   结束再写回（用 `generations` 代际号避免覆盖 `reset` 的结果）。
//! - `cancels`：`run_id -> Arc<AtomicBool>`，`abort` 置位。
//! - `pending`：`approval_id -> oneshot::Sender<ApprovalReply>`，`approve` 派发。
//! - `inflight`：防止同一 `run_id` 并发两轮对话。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tokio::sync::{oneshot, Mutex};

use crate::agent::audit::{self, AuditEntry, AuditLog};
use crate::agent::executor::ToolExecutor;
use crate::agent::prompt;
use crate::agent::tools::tool_specs;
use crate::error::{AppError, AppResult};
use crate::guard::classifier::classify;
use crate::guard::confirm::{self, ConfirmState};
use crate::ipc::dto::{AgentModelInfo, AgentSendRequest, Risk};
use crate::ipc::events::{
    AgentApprovalRequiredEvent, AgentDeltaEvent, AgentDoneEvent, AgentErrorEvent,
    AgentToolCallEvent, AgentToolResultEvent, AGENT_APPROVAL_REQUIRED, AGENT_DELTA, AGENT_DONE,
    AGENT_ERROR, AGENT_TOOL_CALL, AGENT_TOOL_RESULT,
};
use crate::llm::openai::OpenAiCompatProvider;
use crate::llm::provider::LlmProvider;
use crate::llm::types::{ChatMessage, Delta};
use crate::persist::store::PersistStore;
use crate::ssh::manager::SessionManager;

/// tool loop 最大轮数。
const MAX_ITERATIONS: usize = 12;
/// 审批等待上限（超过视为拒绝）。
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);
/// 审批轮询切片的粒度（用于同时检查取消标志）。
const APPROVAL_POLL_SLICE: Duration = Duration::from_millis(250);
/// 单条工具结果回灌模型的最大字符数。
const TOOL_OUTPUT_LIMIT: usize = 6000;

/// 审批派发的消息体：是否放行 + 高危命令的二次确认短语。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApprovalReply {
    pub approved: bool,
    /// `Dangerous` 命令必须带这个短语（精确等于 `confirm::CONFIRM_PHRASE`）
    pub phrase: Option<String>,
}

type PendingMap = Arc<Mutex<HashMap<String, oneshot::Sender<ApprovalReply>>>>;
type HistoryMap = Arc<Mutex<HashMap<String, Vec<ChatMessage>>>>;
type CancelMap = Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>;
type InflightMap = Arc<Mutex<HashMap<String, bool>>>;
type GenerationMap = Arc<Mutex<HashMap<String, u64>>>;

pub struct AgentRuntime {
    sessions: Arc<SessionManager>,
    /// 配置目录（审计日志落盘位置）。
    config_dir: PathBuf,
    /// 审批审计（`<config_dir>/audit.jsonl`）。失败静默，不阻断运维。
    audit: Arc<AuditLog>,
    history: HistoryMap,
    cancels: CancelMap,
    pending: PendingMap,
    inflight: InflightMap,
    generations: GenerationMap,
}

impl AgentRuntime {
    pub fn new(sessions: Arc<SessionManager>, config_dir: PathBuf) -> Self {
        let audit = Arc::new(AuditLog::new(config_dir.clone()));
        Self {
            sessions,
            config_dir,
            audit,
            history: Arc::new(Mutex::new(HashMap::new())),
            cancels: Arc::new(Mutex::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
            inflight: Arc::new(Mutex::new(HashMap::new())),
            generations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 审计日志句柄（供 IPC 提供「查看审计」能力，也供测试断言）。
    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    /// 配置目录（审计文件所在目录）。
    pub fn config_dir(&self) -> &PathBuf {
        &self.config_dir
    }

    /// 发起一轮对话。
    ///
    /// **硬要求**：tool loop 丢到 `tauri::async_runtime::spawn` 后台跑，本函数立即 `Ok(())`，
    /// 否则会阻塞 Tauri IPC 线程。
    pub async fn send(
        &self,
        app: AppHandle,
        req: AgentSendRequest,
        api_key: String,
    ) -> AppResult<()> {
        let run_id = req.run_id.trim().to_string();
        if run_id.is_empty() {
            return Err(AppError::InvalidInput("runId is required".into()));
        }
        let message = req.message.trim().to_string();
        if message.is_empty() {
            return Err(AppError::InvalidInput("message is required".into()));
        }

        // 同一 run 不并发
        {
            let mut g = self.inflight.lock().await;
            if g.get(&run_id).copied().unwrap_or(false) {
                return Err(AppError::Agent(
                    "该对话正在进行中，请等待本轮结束或先中止".into(),
                ));
            }
            g.insert(run_id.clone(), true);
        }

        // 取/建历史，追加 user 消息，然后把历史"取走"到后台任务
        let generation = {
            let mut gen_g = self.generations.lock().await;
            *gen_g.entry(run_id.clone()).or_insert(0)
        };

        let mut messages: Vec<ChatMessage> = {
            let mut h = self.history.lock().await;
            let entry = h.entry(run_id.clone()).or_insert_with(|| {
                let mut sys = prompt::system_prompt();
                sys.push_str(&format!(
                    "\n\n当前会话上下文：SSH session_id = `{}`。调用 run_command 时可省略 session_id。",
                    req.session_id
                ));
                vec![ChatMessage::system(sys)]
            });
            entry.push(ChatMessage::user(message));
            std::mem::take(entry)
        };
        if messages.is_empty() {
            messages.push(ChatMessage::system(prompt::system_prompt()));
        }

        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels
            .lock()
            .await
            .insert(run_id.clone(), cancel.clone());

        let ctx = RunCtx {
            app: app.clone(),
            run_id: run_id.clone(),
            session_id: req.session_id.clone(),
            sessions: self.sessions.clone(),
            pending: self.pending.clone(),
            audit: self.audit.clone(),
            cancel: cancel.clone(),
            api_key,
        };

        let history = self.history.clone();
        let cancels = self.cancels.clone();
        let inflight = self.inflight.clone();
        let generations = self.generations.clone();
        let cleanup_run_id = run_id.clone();

        tauri::async_runtime::spawn(async move {
            let result = run_tool_loop(&ctx, &mut messages).await;

            // 写回历史（仅当没有被 reset 换代）
            let current_gen = *generations
                .lock()
                .await
                .get(&cleanup_run_id)
                .unwrap_or(&generation);
            if current_gen == generation {
                history.lock().await.insert(cleanup_run_id.clone(), messages);
            }

            // 清理运行态
            cancels.lock().await.remove(&cleanup_run_id);
            inflight.lock().await.remove(&cleanup_run_id);

            if let Err(e) = &result {
                emit_error(&ctx.app, &cleanup_run_id, e);
            }
            let reason = match &result {
                Ok(r) => r.clone(),
                Err(_) => "error".to_string(),
            };
            let _ = ctx.app.emit(
                AGENT_DONE,
                AgentDoneEvent {
                    run_id: cleanup_run_id,
                    reason,
                },
            );
        });

        Ok(())
    }

    /// 中断一轮对话。未在运行时也返回 Ok（幂等）。
    pub async fn abort(&self, run_id: &str) -> AppResult<()> {
        let id = run_id.trim();
        if id.is_empty() {
            return Err(AppError::InvalidInput("runId is required".into()));
        }
        if let Some(flag) = self.cancels.lock().await.get(id) {
            flag.store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    /// 审批结果派发。找不到 approval（已超时/已处理）→ NotFound。
    ///
    /// 兼容既有 IPC 签名：不带短语。`Dangerous` 命令会因缺少短语而**不放行**
    /// （fail-safe），真正的高危放行请走 [`Self::approve_with_phrase`]。
    pub async fn approve(&self, approval_id: &str, approved: bool) -> AppResult<()> {
        self.approve_with_phrase(approval_id, approved, None).await
    }

    /// 审批结果派发 + 高危命令的二次确认短语。
    ///
    /// `ipc::agent_approve` 应改为调用本方法，并把 `AgentApproveRequest.confirmPhrase`
    /// 透传进来（见交付说明）。
    pub async fn approve_with_phrase(
        &self,
        approval_id: &str,
        approved: bool,
        phrase: Option<&str>,
    ) -> AppResult<()> {
        let id = approval_id.trim();
        if id.is_empty() {
            return Err(AppError::InvalidInput("approvalId is required".into()));
        }
        let sender = self.pending.lock().await.remove(id);
        match sender {
            Some(tx) => {
                // 接收端可能已超时丢弃，这里忽略 send 失败
                let _ = tx.send(ApprovalReply {
                    approved,
                    phrase: phrase.map(|p| p.to_string()),
                });
                Ok(())
            }
            None => Err(AppError::NotFound(format!(
                "approval {id}（可能已超时或已处理）"
            ))),
        }
    }

    /// 清空该 run 的消息历史（同时换代，避免进行中的循环把它写回来）。
    pub async fn reset(&self, run_id: &str) -> AppResult<()> {
        let id = run_id.trim();
        if id.is_empty() {
            return Err(AppError::InvalidInput("runId is required".into()));
        }
        self.history.lock().await.remove(id);
        let mut gen_g = self.generations.lock().await;
        let e = gen_g.entry(id.to_string()).or_insert(0);
        *e = e.saturating_add(1);
        Ok(())
    }

    /// 探测模型列表（供 `agent_list_models`）。
    pub async fn list_models(
        &self,
        base_url: &str,
        api_key: &str,
    ) -> AppResult<Vec<AgentModelInfo>> {
        if base_url.trim().is_empty() {
            return Err(AppError::InvalidInput("baseUrl is required".into()));
        }
        let provider = OpenAiCompatProvider::new(base_url, api_key, "", 0.0);
        let infos = provider.list_model_infos().await.map_err(|e| match e {
            AppError::Llm(m) => AppError::Llm(m),
            other => AppError::Llm(other.message()),
        })?;
        Ok(infos
            .into_iter()
            .map(|(id, owned_by)| AgentModelInfo { id, owned_by })
            .collect())
    }
}

// ───────────────────────────── 审批结果 ─────────────────────────────

/// 一次审批的完整结论：状态 + 审计决策串 + 可读原因 + 回灌给模型的说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalOutcome {
    /// 二次确认闸门状态（`ReadOnly`/`Mutating` 视为 `Satisfied`）。
    pub state: ConfirmState,
    /// 审计用的决策串（`auto` / `approved` / `rejected` / `timeout` / `aborted`）。
    pub decision: &'static str,
    /// 审计原因（分类原因 + 二次确认结论）。
    pub reason: Option<String>,
    /// 回灌给模型的人话说明（放行时为空）。
    pub user_message: String,
}

impl ApprovalOutcome {
    /// 只读命令自动放行。
    fn auto(classifier_reason: &str) -> Self {
        Self {
            state: ConfirmState::Satisfied,
            decision: audit::decision::AUTO,
            reason: Some(classifier_reason.to_string()),
            user_message: String::new(),
        }
    }

    /// 审批 + 二次确认通过。
    fn approved(risk: Risk, classifier_reason: &str, state: ConfirmState) -> Self {
        Self {
            state,
            decision: audit::decision::APPROVED,
            reason: Some(describe_reason(risk, classifier_reason, state)),
            user_message: String::new(),
        }
    }

    /// 用户点了拒绝。
    fn denied(risk: Risk, classifier_reason: &str) -> Self {
        Self {
            state: ConfirmState::Rejected,
            decision: audit::decision::REJECTED,
            reason: Some(describe_reason(
                risk,
                classifier_reason,
                ConfirmState::Rejected,
            )),
            user_message: "用户拒绝了该命令".to_string(),
        }
    }

    /// 高危命令缺短语 / 短语不匹配。
    fn confirm_failed(
        risk: Risk,
        classifier_reason: &str,
        state: ConfirmState,
        reason_hint: Option<String>,
    ) -> Self {
        let detail = match (reason_hint, confirm::failure_message(state)) {
            (Some(h), Some(f)) => Some(format!("{h}；{f}")),
            (Some(h), None) => Some(h),
            (None, f) => f,
        };
        let hint = detail.clone().unwrap_or_else(|| "二次确认未通过".to_string());
        Self {
            state,
            decision: audit::decision::REJECTED,
            reason: Some(describe_reason(risk, classifier_reason, state)),
            user_message: format!("{hint}，命令未执行"),
        }
    }

    /// 审批超时。
    fn timeout(_risk: Risk, classifier_reason: &str) -> Self {
        Self {
            state: ConfirmState::Timeout,
            decision: audit::decision::TIMEOUT,
            reason: Some(format!(
                "{classifier_reason}｜审批在 {}s 内未响应，已拒绝执行",
                APPROVAL_TIMEOUT.as_secs()
            )),
            user_message: format!(
                "审批超时（{}s 内未响应），命令未执行",
                APPROVAL_TIMEOUT.as_secs()
            ),
        }
    }

    /// 对话被中止（等待审批期间）。
    fn aborted(classifier_reason: &str) -> Self {
        Self {
            state: ConfirmState::Rejected,
            decision: audit::decision::ABORTED,
            reason: Some(format!("{classifier_reason}｜等待审批期间对话被用户中止")),
            user_message: "对话已被用户中止，命令未执行".to_string(),
        }
    }

    /// 是否允许执行。**唯一放行条件是 `state == Satisfied`**。
    pub fn is_allowed(&self) -> bool {
        self.state.is_satisfied()
    }
}

fn describe_reason(risk: Risk, classifier_reason: &str, state: ConfirmState) -> String {
    format!(
        "{}｜二次确认: {}",
        classifier_reason,
        match risk {
            // ReadOnly/Mutating 不涉及短语，这里只标注闸门结论
            Risk::ReadOnly => audit::decision::AUTO,
            Risk::Mutating => "single-approval",
            Risk::Dangerous => state.label(),
        }
    )
}

/// 把一次审批结论写进审计日志（失败静默）。
fn audit_decision(
    audit: &AuditLog,
    session_id: &str,
    command: &str,
    risk: Risk,
    outcome: &ApprovalOutcome,
) {
    let mut entry = AuditEntry::new(session_id, command, risk, outcome.decision);
    entry.reason = outcome.reason.clone();
    audit.record(entry);
}

// ───────────────────────────── 后台 loop 上下文 ─────────────────────────────

struct RunCtx {
    app: AppHandle,
    run_id: String,
    session_id: String,
    sessions: Arc<SessionManager>,
    pending: PendingMap,
    audit: Arc<AuditLog>,
    cancel: Arc<AtomicBool>,
    api_key: String,
}

impl RunCtx {
    fn aborted(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn emit_tool_call(&self, call_id: &str, name: &str, args: &str) {
        let _ = self.app.emit(
            AGENT_TOOL_CALL,
            AgentToolCallEvent {
                run_id: self.run_id.clone(),
                call_id: call_id.to_string(),
                name: name.to_string(),
                args: args.to_string(),
            },
        );
    }

    fn emit_tool_result(&self, call_id: &str, ok: bool, output: &str) {
        let _ = self.app.emit(
            AGENT_TOOL_RESULT,
            AgentToolResultEvent {
                run_id: self.run_id.clone(),
                call_id: call_id.to_string(),
                ok,
                output: clip(output, TOOL_OUTPUT_LIMIT),
            },
        );
    }

    /// 等待一次人工审批（含 `Dangerous` 的二次确认）。**永不返回错误**，
    /// 一切分支都记审计；是否放行由 [`ApprovalOutcome::is_allowed`] 判定。
    async fn request_approval(
        &self,
        command: &str,
        risk: Risk,
        classifier_reason: &str,
    ) -> ApprovalOutcome {
        // 只读命令无需人工介入，直接放行并记 auto
        if matches!(risk, Risk::ReadOnly) {
            let outcome = ApprovalOutcome::auto(classifier_reason);
            audit_decision(&self.audit, &self.session_id, command, risk, &outcome);
            return outcome;
        }

        let approval_id = format!("approval_{}", uuid::Uuid::new_v4());
        let (tx, rx) = oneshot::channel::<ApprovalReply>();
        self.pending.lock().await.insert(approval_id.clone(), tx);

        let _ = self.app.emit(
            AGENT_APPROVAL_REQUIRED,
            AgentApprovalRequiredEvent {
                run_id: self.run_id.clone(),
                approval_id: approval_id.clone(),
                command: command.to_string(),
                risk,
                // 高危命令在 reason 里明确写出「必须输入短语」，前端可据此渲染二次确认输入框
                reason: if matches!(risk, Risk::Dangerous) {
                    format!("{classifier_reason}｜{}", confirm::danger_confirm_hint())
                } else {
                    classifier_reason.to_string()
                },
            },
        );

        let waited = wait_decision(rx, &self.cancel, APPROVAL_TIMEOUT).await;

        // 无论结果如何都要清理槽位（避免泄漏）
        self.pending.lock().await.remove(&approval_id);

        let outcome = decide_outcome(risk, classifier_reason, waited);
        audit_decision(&self.audit, &self.session_id, command, risk, &outcome);
        outcome
    }
}

/// 把「审批等待结果」映射成最终结论 —— 纯函数，便于单测覆盖全部分支。
fn decide_outcome(risk: Risk, classifier_reason: &str, waited: WaitResult) -> ApprovalOutcome {
    match waited {
        WaitResult::Aborted => ApprovalOutcome::aborted(classifier_reason),
        WaitResult::Timeout => ApprovalOutcome::timeout(risk, classifier_reason),
        WaitResult::Reply(reply) => {
            if !reply.approved {
                ApprovalOutcome::denied(risk, classifier_reason)
            } else {
                let state = confirm::check_confirm(risk, reply.phrase.as_deref());
                match state {
                    ConfirmState::Satisfied => {
                        ApprovalOutcome::approved(risk, classifier_reason, state)
                    }
                    ConfirmState::Required | ConfirmState::Rejected => {
                        ApprovalOutcome::confirm_failed(
                            risk,
                            classifier_reason,
                            state,
                            reply.phrase.map(|p| format!("用户输入的短语为 {p:?}")),
                        )
                    }
                    ConfirmState::Timeout => ApprovalOutcome::timeout(risk, classifier_reason),
                }
            }
        }
    }
}

// ───────────────────────────── 等待审批 ─────────────────────────────

enum WaitResult {
    Reply(ApprovalReply),
    Timeout,
    Aborted,
}

/// 等待审批；期间每 250ms 检查一次取消标志，最长 `total`。
async fn wait_decision(
    mut rx: oneshot::Receiver<ApprovalReply>,
    cancel: &AtomicBool,
    total: Duration,
) -> WaitResult {
    let deadline = tokio::time::Instant::now() + total;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return WaitResult::Aborted;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return WaitResult::Timeout;
        }
        let slice = (deadline - now).min(APPROVAL_POLL_SLICE);
        match tokio::time::timeout(slice, &mut rx).await {
            Ok(Ok(reply)) => return WaitResult::Reply(reply),
            Ok(Err(_)) => return WaitResult::Aborted, // 发送端已丢弃（例如 run 被清理）
            Err(_) => continue,                       // 本切片超时，重新检查取消/总超时
        }
    }
}

fn emit_error(app: &AppHandle, run_id: &str, e: &AppError) {
    let _ = app.emit(
        AGENT_ERROR,
        AgentErrorEvent {
            run_id: run_id.to_string(),
            code: e.code().to_string(),
            message: e.message(),
        },
    );
}

// ───────────────────────────── tool loop ─────────────────────────────

async fn run_tool_loop(ctx: &RunCtx, messages: &mut Vec<ChatMessage>) -> AppResult<String> {
    // 配置从磁盘读取（不改 state.rs），api_key 由 IPC 层传入
    let cfg = PersistStore::new().public_config();

    // 模型解析：`active_model`（形如 `provider/model`）优先；为空则回退到
    // 老路径 —— 用户直接填的 base_url + model + 全局 key（等价于 builtin 通道）。
    //
    // 走 `router::resolve` 而不是自己拼 base_url，是为了把「渠道 Key 覆盖、
    // 本机通道免 Key、端点归一化」这些规则收敛到一处，避免两套逻辑漂移。
    let (base_url, model, api_key) = if cfg.llm.active_model.trim().is_empty() {
        if cfg.llm.model.trim().is_empty() {
            return Err(AppError::Agent(
                "未配置模型：请在设置 → Agent 中选择模型，或填写 LLM model".into(),
            ));
        }
        (
            cfg.llm.base_url.clone(),
            cfg.llm.model.clone(),
            Some(ctx.api_key.clone()),
        )
    } else {
        let channels: std::collections::HashMap<String, crate::llm::router::ChannelConfig> = cfg
            .llm
            .channels
            .iter()
            .map(|c| (c.provider_id.clone(), c.clone().into()))
            .collect();
        // 从持久层取渠道明文 Key（public_config 只暴露 has_key）
        let store = PersistStore::new();
        let mut channels_with_key = channels;
        for ch in store.channel_api_keys() {
            if let Some(cfg_ch) = channels_with_key.get_mut(&ch.0) {
                cfg_ch.api_key = Some(ch.1);
            }
        }
        let default_cfg = crate::llm::router::DefaultLlmConfig {
            base_url: cfg.llm.base_url.clone(),
            model: cfg.llm.model.clone(),
            api_key: ctx.api_key.clone(),
        };
        let target = crate::llm::router::resolve(&cfg.llm.active_model, &channels_with_key, &default_cfg)?;
        (
            target.base_url,
            target.model,
            target
                .api_key
                .filter(|k| !k.trim().is_empty())
                .or_else(|| {
                    let k = ctx.api_key.trim().to_string();
                    (!k.is_empty()).then_some(k)
                }),
        )
    };

    let provider = OpenAiCompatProvider::new(
        base_url,
        api_key.clone().unwrap_or_default(),
        model,
        cfg.llm.temperature,
    );
    let tools = tool_specs();
    let executor = ToolExecutor::new(ctx.sessions.clone());

    let mut iterations = 0usize;
    let mut last_reason: String;

    loop {
        if iterations >= MAX_ITERATIONS {
            last_reason = "max_iterations".to_string();
            break;
        }
        if ctx.aborted() {
            last_reason = "aborted".to_string();
            break;
        }
        iterations += 1;

        let app = ctx.app.clone();
        let run_id = ctx.run_id.clone();
        let mut sink = move |delta: Delta| {
            if let Delta::Text(t) = delta {
                if t.is_empty() {
                    return;
                }
                let _ = app.emit(
                    AGENT_DELTA,
                    AgentDeltaEvent {
                        run_id: run_id.clone(),
                        delta: t,
                    },
                );
            }
        };

        let outcome = provider.stream_chat(messages, &tools, &mut sink).await?;

        // 记录 assistant 消息（带 tool_calls，供下一轮 tool 消息配对）
        let has_tools = !outcome.tool_calls.is_empty();
        let content = if outcome.content.is_empty() {
            None
        } else {
            Some(outcome.content.clone())
        };
        messages.push(ChatMessage {
            role: "assistant".into(),
            content,
            tool_calls: if has_tools {
                Some(outcome.tool_calls.clone())
            } else {
                None
            },
            tool_call_id: None,
        });

        last_reason = if outcome.finish_reason.is_empty() {
            "stop".to_string()
        } else {
            outcome.finish_reason.clone()
        };

        if outcome.finish_reason == "tool_calls" && has_tools {
            for tc in &outcome.tool_calls {
                if ctx.aborted() {
                    // 被中止的命令（未经审批）也留一条审计，保证「全流程留痕」
                    audit_aborted_command(ctx, &tc.function.name, &tc.function.arguments);
                    messages.push(ChatMessage::tool(
                        tc.id.clone(),
                        "对话已被用户中止，该工具调用未执行",
                    ));
                    continue;
                }
                let result = handle_tool_call(ctx, &executor, tc).await;
                // 省 Token：先把工具输出过 RTK 压缩，再喂回模型。
                //
                // 顺序很关键 —— 必须**先压缩、后 clip**：RTK 提升信息密度后再按字符
                // 上限截断，模型拿到的是压缩后的完整视图；若先截断，砍掉的恰是最前面
                // 的关键内容。`compress_and_account` 内部已保证「报错不压缩 / 小于下限
                // 不压缩 / 压缩后更大则回退 / 幂等 / 失败原样放行」，且顺带累计统计。
                let compressed = crate::token::compress_and_account(&result.1);
                messages.push(ChatMessage::tool(
                    tc.id.clone(),
                    clip(&compressed, TOOL_OUTPUT_LIMIT),
                ));
                // 事件推给前端的是**原始输出**：压缩只作用于送模型的上下文，
                // 不应影响人看到的审计信息。
                ctx.emit_tool_result(&tc.id, result.0, &result.1);
            }
            // 继续下一轮
            continue;
        }

        break;
    }

    Ok(last_reason)
}

/// `run_command` 被中止时补一条审计（`aborted`），保证命令不静默消失。
fn audit_aborted_command(ctx: &RunCtx, tool_name: &str, args: &str) {
    if tool_name != "run_command" {
        return;
    }
    let command = match extract_command(args) {
        Some(c) => c,
        None => return,
    };
    let cls = classify(&command);
    let outcome = ApprovalOutcome::aborted(&cls.reason);
    audit_decision(&ctx.audit, &ctx.session_id, &command, cls.risk, &outcome);
}

/// 从工具参数 JSON 里取 `command`。
fn extract_command(args: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    v.get("command")
        .and_then(|c| c.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// 处理单个工具调用，返回 `(ok, output)`。
async fn handle_tool_call(
    ctx: &RunCtx,
    executor: &ToolExecutor,
    tc: &crate::llm::types::ToolCall,
) -> (bool, String) {
    ctx.emit_tool_call(&tc.id, &tc.function.name, &tc.function.arguments);

    let args: serde_json::Value = match serde_json::from_str(&tc.function.arguments) {
        Ok(v) => v,
        Err(e) => {
            return (
                false,
                format!(
                    "工具参数不是合法 JSON（模型输出格式错误）：{e}；原始参数：{}",
                    clip(&tc.function.arguments, 400)
                ),
            );
        }
    };

    match tc.function.name.as_str() {
        "run_command" => {
            let command = args
                .get("command")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if command.is_empty() {
                return (false, "run_command 缺少 command 参数".to_string());
            }
            let session_id = args
                .get("session_id")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| ctx.session_id.clone());

            // 风险分级（分类逻辑不变，只在其之上加审批/二次确认/审计）
            let cls = classify(&command);
            let outcome = ctx
                .request_approval(&command, cls.risk, &cls.reason)
                .await;
            if !outcome.is_allowed() {
                return (false, outcome.user_message);
            }

            match executor.run_command(&session_id, &command).await {
                Ok(out) => {
                    let body = out.trim();
                    if body.is_empty() {
                        (true, format!("（命令 `{command}` 执行成功，无输出）"))
                    } else {
                        (true, clip(body, TOOL_OUTPUT_LIMIT))
                    }
                }
                Err(e) => (false, format!("命令执行失败：{}", e.message())),
            }
        }

        "list_sessions" => {
            let sessions = ctx.sessions.list().await;
            if sessions.is_empty() {
                return (true, "当前没有已连接的 SSH 会话".to_string());
            }
            let mut lines: Vec<String> = Vec::new();
            lines.push(format!("共 {} 个会话：", sessions.len()));
            for s in sessions {
                lines.push(format!(
                    "- session_id={} {}@{}:{} state={:?}",
                    s.id, s.username, s.host, s.port, s.state
                ));
            }
            (true, lines.join("\n"))
        }

        other => (
            false,
            format!("未知工具 `{other}`，本次调用未执行"),
        ),
    }
}

fn clip(s: &str, limit: usize) -> String {
    let count = s.chars().count();
    if count <= limit {
        return s.to_string();
    }
    let mut out: String = s.chars().take(limit).collect();
    out.push_str(&format!("\n…（输出已截断，原始长度 {count} 字符）"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn rt(dir: &Path) -> AgentRuntime {
        AgentRuntime::new(Arc::new(SessionManager::new()), dir.to_path_buf())
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sshdeck-runtime-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn clip_truncates_long_output() {
        let s = "a".repeat(7000);
        let out = clip(&s, TOOL_OUTPUT_LIMIT);
        assert!(out.contains("已截断"));
        assert!(out.chars().count() <= TOOL_OUTPUT_LIMIT + 60);
        let short = "hello";
        assert_eq!(clip(short, 100), short);
    }

    #[tokio::test]
    async fn approve_dispatches_decision() {
        let dir = temp_dir("approve");
        let rt = rt(&dir);

        let (tx, rx) = oneshot::channel::<ApprovalReply>();
        rt.pending.lock().await.insert("ap-1".to_string(), tx);
        rt.approve("ap-1", true).await.unwrap();
        let reply = rx.await.unwrap();
        assert!(reply.approved);
        assert!(reply.phrase.is_none(), "兼容旧签名时不带短语");

        // 重复派发 → NotFound
        assert!(rt.approve("ap-1", false).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn approve_with_phrase_passes_phrase() {
        let dir = temp_dir("phrase");
        let rt = rt(&dir);

        let (tx, rx) = oneshot::channel::<ApprovalReply>();
        rt.pending.lock().await.insert("ap-2".to_string(), tx);
        rt.approve_with_phrase("ap-2", true, Some(confirm::CONFIRM_PHRASE))
            .await
            .unwrap();
        let reply = rx.await.unwrap();
        assert!(reply.approved);
        assert_eq!(reply.phrase.as_deref(), Some(confirm::CONFIRM_PHRASE));

        // 空 id → InvalidInput
        assert!(rt.approve_with_phrase("  ", true, None).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reset_bumps_generation() {
        let dir = temp_dir("reset");
        let rt = rt(&dir);
        rt.reset("run-1").await.unwrap();
        assert_eq!(*rt.generations.lock().await.get("run-1").unwrap(), 1);
        assert!(rt.history.lock().await.get("run-1").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn abort_sets_flag() {
        let dir = temp_dir("abort");
        let rt = rt(&dir);
        let flag = Arc::new(AtomicBool::new(false));
        rt.cancels
            .lock()
            .await
            .insert("run-x".to_string(), flag.clone());
        rt.abort("run-x").await.unwrap();
        assert!(flag.load(Ordering::SeqCst));
        // 未注册的 run 也幂等
        rt.abort("run-y").await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn wait_decision_wins_when_sent() {
        let (tx, rx) = oneshot::channel::<ApprovalReply>();
        let cancel = AtomicBool::new(false);
        tokio::spawn(async move {
            let _ = tx.send(ApprovalReply {
                approved: true,
                phrase: None,
            });
        });
        let waited = wait_decision(rx, &cancel, Duration::from_secs(2)).await;
        assert!(matches!(waited, WaitResult::Reply(r) if r.approved));
    }

    #[tokio::test]
    async fn wait_decision_aborts_on_cancel() {
        let (_tx, rx) = oneshot::channel::<ApprovalReply>();
        let cancel = AtomicBool::new(true);
        let waited = wait_decision(rx, &cancel, Duration::from_secs(2)).await;
        assert!(matches!(waited, WaitResult::Aborted));
    }

    #[tokio::test]
    async fn wait_decision_times_out() {
        let (_tx, rx) = oneshot::channel::<ApprovalReply>();
        let cancel = AtomicBool::new(false);
        let waited = wait_decision(rx, &cancel, Duration::from_millis(1)).await;
        assert!(matches!(waited, WaitResult::Timeout));
    }

    /// 五条审计决策串都能落盘并读回（完整覆盖 §5.3 的 decision 取值）。
    #[test]
    fn audit_decision_covers_all_branches() {
        let dir = temp_dir("audit-branches");
        let log = AuditLog::with_path(dir.join("audit.jsonl"));

        let cases: Vec<(&str, Risk, ApprovalOutcome)> = vec![
            (
                "ls -la",
                Risk::ReadOnly,
                ApprovalOutcome::auto("全部子命令均命中只读白名单"),
            ),
            (
                "systemctl restart nginx",
                Risk::Mutating,
                ApprovalOutcome::approved(
                    Risk::Mutating,
                    "会重启服务",
                    ConfirmState::Satisfied,
                ),
            ),
            (
                "rm -rf /",
                Risk::Dangerous,
                ApprovalOutcome::denied(Risk::Dangerous, "删除根目录"),
            ),
            (
                "reboot",
                Risk::Dangerous,
                ApprovalOutcome::timeout(Risk::Dangerous, "重启主机"),
            ),
            (
                "mkfs.ext4 /dev/sda1",
                Risk::Dangerous,
                ApprovalOutcome::aborted("格式化块设备"),
            ),
        ];

        for (cmd, risk, outcome) in &cases {
            audit_decision(&log, "sess-1", cmd, *risk, outcome);
        }

        let tail = log.tail(cases.len());
        assert_eq!(tail.len(), cases.len());
        let decisions: Vec<&str> = tail.iter().map(|e| e.decision.as_str()).collect();
        assert_eq!(
            decisions,
            vec!["auto", "approved", "rejected", "timeout", "aborted"],
            "五条决策串必须齐全"
        );
        assert_eq!(tail[1].risk, "mutating");
        assert_eq!(tail[2].risk, "dangerous");
        assert_eq!(tail[0].session_id, "sess-1");
        assert_eq!(tail[2].command, "rm -rf /");
        assert_eq!(tail[2].risk, "dangerous");
        assert!(
            tail[2]
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("二次确认: rejected"),
            "审计 reason 应写明二次确认结论，实际: {:?}",
            tail[2].reason
        );
        // 所有审计行都能被 JSON 解析回来（落盘格式正确）
        assert_eq!(log.count(), cases.len());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 高危命令：只差一个字不放行；精确短语才放行。
    #[test]
    fn dangerous_outcome_requires_exact_phrase() {
        let allowed = ApprovalOutcome::approved(
            Risk::Dangerous,
            "删除根目录",
            ConfirmState::Satisfied,
        );
        assert!(allowed.is_allowed());

        for bad in [None, Some("确认"), Some("确认执行吧")] {
            let state = confirm::check_confirm(Risk::Dangerous, bad);
            let denied = ApprovalOutcome::confirm_failed(
                Risk::Dangerous,
                "删除根目录",
                state,
                bad.map(|p| format!("短语 {p:?}")),
            );
            assert!(!denied.is_allowed(), "短语 {bad:?} 不得放行");
            assert_eq!(denied.decision, "rejected");
            assert!(denied.user_message.contains("命令未执行"));
        }

        // 精确短语放行
        let ok = confirm::check_confirm(Risk::Dangerous, Some(confirm::CONFIRM_PHRASE));
        assert_eq!(ok, ConfirmState::Satisfied);
    }

    /// `decide_outcome` 的 `Dangerous` 全分支（纯函数，不需要 AppHandle）。
    #[test]
    fn dangerous_pipeline_all_branches() {
        let reason = "删除根目录";

        // 1) 批准 + 精确短语 → 放行
        let ok = decide_outcome(
            Risk::Dangerous,
            reason,
            WaitResult::Reply(ApprovalReply {
                approved: true,
                phrase: Some(confirm::CONFIRM_PHRASE.to_string()),
            }),
        );
        assert!(ok.is_allowed());
        assert_eq!(ok.decision, "approved");
        assert_eq!(ok.state, ConfirmState::Satisfied);

        // 2) 批准 + 短语缺失 → Required（拦截，且是可补填的「未完成」态）
        let missing = decide_outcome(
            Risk::Dangerous,
            reason,
            WaitResult::Reply(ApprovalReply {
                approved: true,
                phrase: None,
            }),
        );
        assert!(!missing.is_allowed(), "缺短语不得放行");
        assert_eq!(missing.decision, "rejected");
        assert_eq!(missing.state, ConfirmState::Required);
        assert!(missing.user_message.contains("确认执行"), "提示必须写明短语");

        // 3) 批准 + 短语错一个标点 → Rejected
        let wrong = decide_outcome(
            Risk::Dangerous,
            reason,
            WaitResult::Reply(ApprovalReply {
                approved: true,
                phrase: Some("确认执行。".to_string()),
            }),
        );
        assert!(!wrong.is_allowed(), "近似短语不得放行");
        assert_eq!(wrong.decision, "rejected");
        assert_eq!(wrong.state, ConfirmState::Rejected);

        // 4) 用户拒绝（即使带了正确短语）→ Rejected
        let deny = decide_outcome(
            Risk::Dangerous,
            reason,
            WaitResult::Reply(ApprovalReply {
                approved: false,
                phrase: Some(confirm::CONFIRM_PHRASE.to_string()),
            }),
        );
        assert!(!deny.is_allowed());
        assert_eq!(deny.decision, "rejected");
        assert_eq!(deny.user_message, "用户拒绝了该命令");

        // 5) 超时 → timeout
        let to = decide_outcome(Risk::Dangerous, reason, WaitResult::Timeout);
        assert!(!to.is_allowed());
        assert_eq!(to.decision, "timeout");
        assert_eq!(to.state, ConfirmState::Timeout);

        // 6) 中止 → aborted
        let ab = decide_outcome(Risk::Dangerous, reason, WaitResult::Aborted);
        assert!(!ab.is_allowed());
        assert_eq!(ab.decision, "aborted");
    }

    /// `Mutating` 单次审批即可放行（不需要短语），`ReadOnly` 自动放行。
    #[test]
    fn lower_risk_pipeline_needs_no_phrase() {
        let mutating_ok = decide_outcome(
            Risk::Mutating,
            "会重启服务",
            WaitResult::Reply(ApprovalReply {
                approved: true,
                phrase: None,
            }),
        );
        assert!(mutating_ok.is_allowed(), "Mutating 单次审批即放行");
        assert_eq!(mutating_ok.decision, "approved");

        let mutating_denied = decide_outcome(
            Risk::Mutating,
            "会重启服务",
            WaitResult::Reply(ApprovalReply {
                approved: false,
                phrase: None,
            }),
        );
        assert!(!mutating_denied.is_allowed());
        assert_eq!(mutating_denied.decision, "rejected");

        // ReadOnly 根本不进审批队列
        let auto = ApprovalOutcome::auto("全部子命令均命中只读白名单");
        assert!(auto.is_allowed());
        assert_eq!(auto.decision, "auto");
    }

    #[test]
    fn extract_command_reads_json_args() {
        assert_eq!(
            extract_command("{\"command\":\"  df -h  \",\"session_id\":\"s1\"}"),
            Some("df -h".to_string())
        );
        assert_eq!(extract_command("{\"command\":\"\"}"), None);
        assert_eq!(extract_command("{\"command\":\"   \"}"), None);
        assert_eq!(extract_command("not json"), None);
        assert_eq!(extract_command("{\"session_id\":\"s1\"}"), None);
    }

    #[test]
    fn runtime_audit_path_is_under_config_dir() {
        let dir = temp_dir("path");
        let rt = rt(&dir);
        assert_eq!(
            rt.audit().path(),
            dir.join(crate::agent::audit::AUDIT_FILE_NAME)
        );
        assert_eq!(rt.config_dir(), &dir);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
