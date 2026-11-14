//! 端口转发守护：让规则「跨断线 / 跨重启」持续生效。
//!
//! ## 与 `ForwardRegistry` 的分工
//!
//! - `ForwardRegistry` 只管**运行实例**：在某个会话上启动规则、维护 `-L`/`-R`
//!   任务、广播状态。
//! - `ForwardSupervisor` 管**规则定义**：落盘到 `state.json`、在会话消失后
//!   换一条连接把规则重新拉起来。
//!
//! ## 恢复策略
//!
//! | 场景 | 行为 |
//! |---|---|
//! | 会话正常关闭 / 网络中断 | 规则置 `reconnecting`；优先复用同主机的其他活会话；没有则建**隐藏后台连接**重连（指数退避 1→2→4…→`maxBackoff`） |
//! | 应用重启 | 读取 `state.json`，`autoStart` 的规则立即尝试恢复；`pending` 表示还没拿到可用连接 |
//! | 认证失败 / 主机密钥被拒 | 判定为**不可重试**：置 `pending` 并保留原因，等用户登录该主机或手动重试，避免把账号锁死 |
//! | 本地端口被占用 / 服务端拒绝 `-R` | 规则保留为 `failed` + 原因，下次会话重建或手动重试时再试 |
//!
//! ## 凭据边界（重要）
//!
//! **密码与私钥口令从不写磁盘。** 运行时凭据来自「最近一次成功登录」的内存缓存，
//! 因此：
//! - 同一次运行内断线 → 自动重连（缓存命中）；
//! - 应用重启后 → 只有**无私钥口令的 key 登录**能自动恢复，其余规则停在
//!   `pending`，一旦用户登录该主机立刻自动接管（0 次额外操作）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tauri::AppHandle;
use tokio::sync::Mutex;

use crate::error::{AppError, AppResult};
use crate::forward::registry::ForwardRegistry;
use crate::ipc::dto::{
    AuthSpec, ConnectRequest, ForwardInfo, ForwardRequest, ForwardState, PersistedForward,
};
use crate::persist::store::{now_millis, PersistStore};
use crate::ssh::manager::SessionManager;
use crate::ssh::session::SessionEntry;

/// 守护设置。
#[derive(Debug, Clone, Copy)]
pub struct SupervisorSettings {
    pub auto_restore: bool,
    /// 重连退避上限
    pub max_backoff: Duration,
}

impl Default for SupervisorSettings {
    fn default() -> Self {
        Self {
            auto_restore: true,
            max_backoff: Duration::from_secs(30),
        }
    }
}

/// 一条持久化规则的运行态。
struct RuleRuntime {
    def: PersistedForward,
    /// 当前承载它的会话（`None` = 还没跑起来）
    session_id: Option<String>,
    state: ForwardState,
    error: Option<String>,
}

/// 最近一次成功登录留下的凭据（只在内存里）。
#[derive(Clone)]
struct CachedAuth {
    req: ConnectRequest,
    host_id: Option<String>,
}

pub struct ForwardSupervisor {
    sessions: Arc<SessionManager>,
    forwards: Arc<ForwardRegistry>,
    persist: Arc<PersistStore>,
    app: std::sync::Mutex<Option<AppHandle>>,
    rules: Mutex<HashMap<String, RuleRuntime>>,
    /// host_target -> 最近一次成功登录的凭据
    creds: Mutex<HashMap<String, CachedAuth>>,
    /// 正在后台建连的 host_target（去重，避免同一主机并发重连）
    connecting: Mutex<HashSet<String>>,
    settings: std::sync::RwLock<SupervisorSettings>,
}

impl ForwardSupervisor {
    pub fn new(
        sessions: Arc<SessionManager>,
        forwards: Arc<ForwardRegistry>,
        persist: Arc<PersistStore>,
    ) -> Self {
        Self {
            sessions,
            forwards,
            persist,
            app: std::sync::Mutex::new(None),
            rules: Mutex::new(HashMap::new()),
            creds: Mutex::new(HashMap::new()),
            connecting: Mutex::new(HashSet::new()),
            settings: std::sync::RwLock::new(SupervisorSettings::default()),
        }
    }

    pub fn attach_app(&self, app: AppHandle) {
        match self.app.lock() {
            Ok(mut guard) => *guard = Some(app),
            Err(poisoned) => *poisoned.into_inner() = Some(app),
        }
    }

    fn app(&self) -> Option<AppHandle> {
        match self.app.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn apply_settings(&self, auto_restore: bool, max_backoff_secs: u32) {
        let next = SupervisorSettings {
            auto_restore,
            max_backoff: Duration::from_secs(max_backoff_secs.clamp(1, 300) as u64),
        };
        match self.settings.write() {
            Ok(mut guard) => *guard = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }

    fn settings(&self) -> SupervisorSettings {
        match self.settings.read() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    // ───────────────────────── 生命周期钩子 ─────────────────────────

    /// 启动时恢复全部持久化规则（`autoStart` 的立即尝试拉起）。
    pub async fn restore_all(self: &Arc<Self>) {
        let defs = self.persist.load_forwards();
        if defs.is_empty() {
            return;
        }
        let auto: Vec<PersistedForward> = {
            let mut rules = self.rules.lock().await;
            let mut auto = Vec::new();
            for def in defs {
                if def.auto_start {
                    auto.push(def.clone());
                }
                rules.insert(
                    def.id.clone(),
                    RuleRuntime {
                        def,
                        session_id: None,
                        state: ForwardState::Pending,
                        error: None,
                    },
                );
            }
            auto
        };

        let total = self.rules.lock().await.len();
        tracing::info!(total, auto = auto.len(), "restoring persisted forwards");

        if !self.settings().auto_restore {
            tracing::info!("forward auto-restore disabled; rules stay pending");
            return;
        }
        for def in auto {
            self.ensure_available(&def).await;
        }
    }

    /// 用户成功登录：缓存凭据，并把该主机名下 `pending` 的规则接管过来。
    pub async fn on_session_connected(
        self: &Arc<Self>,
        entry: &Arc<SessionEntry>,
        req: &ConnectRequest,
    ) {
        let info = entry.info.lock().await.clone();
        let target = host_target(&info.host, info.port, &info.username);

        let template = ConnectRequest {
            host_id: entry.host_id.clone().or_else(|| req.host_id.clone()),
            host: info.host.clone(),
            port: info.port,
            username: info.username.clone(),
            auth: req.auth.clone(),
            cols: 80,
            rows: 24,
            trust_host_key: None,
        };
        self.creds.lock().await.insert(
            target.clone(),
            CachedAuth {
                req: template,
                host_id: entry.host_id.clone(),
            },
        );

        let ids = self.rule_ids_for(&target, true).await;
        for id in ids {
            self.start_on(&id, &info.id).await;
        }
    }

    /// 记录一条新启动的规则（`forward_start` 之后调用）。
    pub async fn remember(self: &Arc<Self>, info: &ForwardInfo) {
        let def = PersistedForward {
            id: info.id.clone(),
            host_id: info.host_id.clone(),
            host: info.host.clone(),
            port: info.port,
            username: info.username.clone(),
            kind: info.kind,
            listen_host: info.listen_host.clone(),
            listen_port: info.listen_port,
            remote_host: info.remote_host.clone(),
            remote_port: info.remote_port,
            auto_start: info.auto_start,
            created_at: info.created_at,
        };
        if let Err(e) = self.persist.upsert_forward(def.clone()) {
            tracing::warn!(rule = %def.id, error = %e, "persist forward rule failed");
            return;
        }
        self.rules.lock().await.insert(
            def.id.clone(),
            RuleRuntime {
                def,
                session_id: Some(info.session_id.clone()),
                state: info.state,
                error: info.error.clone(),
            },
        );
        self.watch_session(info.id.clone(), info.session_id.clone());
    }

    /// 规则被修改（注册表已完成原地重启）：同步落盘并重挂 watcher。
    pub async fn remember_update(self: &Arc<Self>, info: &ForwardInfo) {
        self.remember(info).await;
    }

    /// 修改一条**尚未运行**（`pending`）的规则：只改定义并尝试拉起。
    pub async fn update_definition(
        self: &Arc<Self>,
        rule_id: &str,
        patch: crate::ipc::dto::ForwardPatch,
    ) -> AppResult<ForwardInfo> {
        let def = {
            let mut rules = self.rules.lock().await;
            let rule = rules
                .get_mut(rule_id)
                .ok_or_else(|| AppError::NotFound(format!("forward rule {rule_id}")))?;
            if let Some(v) = patch.kind {
                rule.def.kind = v;
            }
            if let Some(v) = patch.listen_host {
                rule.def.listen_host = crate::forward::local::normalize_listen_host(&v);
            }
            if let Some(v) = patch.listen_port {
                rule.def.listen_port = v;
            }
            if let Some(v) = patch.remote_host {
                rule.def.remote_host = v.trim().to_string();
            }
            if let Some(v) = patch.remote_port {
                rule.def.remote_port = v;
            }
            rule.state = ForwardState::Pending;
            rule.error = None;
            rule.def.clone()
        };

        self.persist.upsert_forward(def.clone())?;
        self.ensure_available(&def).await;

        let rules = self.rules.lock().await;
        rules
            .get(rule_id)
            .map(pending_info)
            .ok_or_else(|| AppError::NotFound(format!("forward rule {rule_id}")))
    }

    /// 规则被删除：停止跟踪并删除落盘定义。
    pub async fn forget(&self, rule_id: &str) {
        self.rules.lock().await.remove(rule_id);
        if let Err(e) = self.persist.delete_forward(rule_id) {
            tracing::warn!(rule = rule_id, error = %e, "delete persisted forward failed");
        }
    }

    /// 手动重试一条 `pending` / `failed` 规则。
    pub async fn retry(self: &Arc<Self>, rule_id: &str) -> AppResult<()> {
        let def = {
            let rules = self.rules.lock().await;
            rules
                .get(rule_id)
                .map(|r| r.def.clone())
                .ok_or_else(|| AppError::NotFound(format!("forward rule {rule_id}")))?
        };
        self.ensure_available(&def).await;
        Ok(())
    }

    /// 把持久化规则与运行实例合并成一份列表（`pending` 规则也要能看到）。
    pub async fn merge(&self, running: Vec<ForwardInfo>) -> Vec<ForwardInfo> {
        let mut by_id: HashMap<String, ForwardInfo> =
            running.into_iter().map(|f| (f.id.clone(), f)).collect();
        let rules = self.rules.lock().await;
        for rule in rules.values() {
            match by_id.get_mut(&rule.def.id) {
                Some(info) => {
                    // 运行实例还在，但守护层知道它正在重连 / 等凭据
                    if matches!(
                        rule.state,
                        ForwardState::Reconnecting | ForwardState::Pending | ForwardState::Starting
                    ) {
                        info.state = rule.state;
                        info.error = rule.error.clone();
                    }
                }
                None => {
                    by_id.insert(rule.def.id.clone(), pending_info(rule));
                }
            }
        }
        let mut out: Vec<ForwardInfo> = by_id.into_values().collect();
        out.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        out
    }

    // ───────────────────────── 内部：恢复 ─────────────────────────

    async fn rule_ids_for(&self, target: &str, skip_running: bool) -> Vec<String> {
        let rules = self.rules.lock().await;
        rules
            .values()
            .filter(|r| {
                r.def.host_target() == target
                    && !(skip_running && r.state == ForwardState::Running)
            })
            .map(|r| r.def.id.clone())
            .collect()
    }

    /// 确保该主机有一条可用会话承载规则：优先现有会话，否则后台建连。
    async fn ensure_available(self: &Arc<Self>, def: &PersistedForward) {
        if let Some(entry) = self
            .sessions
            .find_live(&def.host, def.port, &def.username)
            .await
        {
            // 只拉起「没在跑」的规则：Running 的规则已在别的会话 / 链路上工作，
            // 再 start 一次会生成重复的转发实例。
            let ids = self.rule_ids_for(&def.host_target(), true).await;
            let session_id = entry.session_id().await;
            for id in ids {
                self.start_on(&id, &session_id).await;
            }
            return;
        }
        self.spawn_connect_loop(def.host_target());
    }

    fn spawn_connect_loop(self: &Arc<Self>, target: String) {
        let sup = self.clone();
        tokio::spawn(async move {
            if !sup.connecting.lock().await.insert(target.clone()) {
                return;
            }
            let result = sup.connect_loop(&target).await;
            sup.connecting.lock().await.remove(&target);
            if let Err(e) = result {
                tracing::warn!(host = %target, error = %e, "forward link restore failed");
            }
        });
    }

    async fn connect_loop(self: &Arc<Self>, target: &str) -> AppResult<()> {
        let settings = self.settings();
        let mut attempt: u32 = 0;

        loop {
            // 规则被删光了就没必要继续
            if self.rule_ids_for(target, false).await.is_empty() {
                return Ok(());
            }

            let cred = match self.creds.lock().await.get(target).cloned() {
                Some(c) => Some(c),
                None => {
                    // 重启后没有内存凭据：只能等用户登录，或走「无私钥口令」的 key
                    match self.profile_auth(target).await {
                        Some(c) => {
                            self.creds.lock().await.insert(target.to_string(), c.clone());
                            Some(c)
                        }
                        None => None,
                    }
                }
            };

            let Some(cred) = cred else {
                self.mark_host(
                    target,
                    ForwardState::Pending,
                    Some("waiting for credentials: log in to this host once".into()),
                )
                .await;
                return Ok(());
            };

            match self.try_connect(target, cred).await {
                Ok(()) => return Ok(()),
                Err(e) if is_fatal(&e) => {
                    self.mark_host(target, ForwardState::Pending, Some(e.message().to_string()))
                        .await;
                    return Ok(());
                }
                Err(e) => {
                    let delay = backoff(attempt, settings.max_backoff);
                    let msg = format!("reconnect failed: {}", e.message());
                    self.mark_host(target, ForwardState::Reconnecting, Some(msg))
                        .await;
                    tracing::info!(
                        host = %target,
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        "retry forward link after backoff"
                    );
                    tokio::time::sleep(delay).await;
                    attempt = attempt.saturating_add(1);
                }
            }
        }
    }

    /// 建一条隐藏连接并把该主机的规则全部拉起来。
    async fn try_connect(self: &Arc<Self>, target: &str, cred: CachedAuth) -> AppResult<()> {
        let ids = self.rule_ids_for(target, true).await;
        if ids.is_empty() {
            return Ok(());
        }

        let entry = self
            .sessions
            .connect_headless(cred.req, cred.host_id)
            .await?;
        let session_id = entry.session_id().await;
        tracing::info!(
            host = %target,
            session = %session_id,
            rules = ids.len(),
            "forward link restored"
        );
        for id in ids {
            self.start_on(&id, &session_id).await;
        }
        Ok(())
    }

    /// 在指定会话上启动一条规则，并挂上「会话结束 → 恢复」的 watcher。
    async fn start_on(self: &Arc<Self>, rule_id: &str, session_id: &str) {
        let Some(def) = ({
            let mut rules = self.rules.lock().await;
            match rules.get_mut(rule_id) {
                Some(rule)
                    if rule.state == ForwardState::Running
                        && rule.session_id.as_deref() == Some(session_id) =>
                {
                    // 已经在这条会话上跑着，别重复 start（会多出一个监听/通道）
                    None
                }
                Some(rule) => {
                    rule.session_id = Some(session_id.to_string());
                    rule.state = ForwardState::Starting;
                    rule.error = None;
                    Some(rule.def.clone())
                }
                None => None,
            }
        }) else {
            return;
        };

        let Some(app) = self.app() else {
            tracing::warn!("forward supervisor has no AppHandle yet; skip start");
            return;
        };

        let req = ForwardRequest {
            session_id: session_id.to_string(),
            kind: def.kind,
            listen_host: def.listen_host.clone(),
            listen_port: def.listen_port,
            remote_host: def.remote_host.clone(),
            remote_port: def.remote_port,
        };

        match self.forwards.start(app, req).await {
            Ok(info) => {
                let mut rules = self.rules.lock().await;
                if let Some(rule) = rules.get_mut(rule_id) {
                    rule.state = info.state;
                    rule.error = info.error.clone();
                }
                drop(rules);
                tracing::info!(rule = rule_id, state = ?info.state, "forward rule bound to link");
            }
            Err(e) => {
                let mut rules = self.rules.lock().await;
                if let Some(rule) = rules.get_mut(rule_id) {
                    rule.state = ForwardState::Pending;
                    rule.error = Some(e.message().to_string());
                }
                tracing::warn!(rule = rule_id, error = %e, "start restored forward failed");
                return;
            }
        }

        self.watch_session(rule_id.to_string(), session_id.to_string());
    }

    /// 监听会话结束：规则进入 `reconnecting` 并触发恢复。
    fn watch_session(self: &Arc<Self>, rule_id: String, session_id: String) {
        let sup = self.clone();
        tokio::spawn(async move {
            let Ok(entry) = sup.sessions.get(&session_id).await else {
                return;
            };
            let mut closed = entry.closed.subscribe();
            if *closed.borrow() {
                sup.on_session_lost(&rule_id, &session_id).await;
                return;
            }
            loop {
                match closed.changed().await {
                    Ok(()) => {
                        if *closed.borrow() {
                            sup.on_session_lost(&rule_id, &session_id).await;
                            return;
                        }
                    }
                    Err(_) => {
                        sup.on_session_lost(&rule_id, &session_id).await;
                        return;
                    }
                }
            }
        });
    }

    async fn on_session_lost(self: &Arc<Self>, rule_id: &str, session_id: &str) {
        let Some(def) = ({
            let mut rules = self.rules.lock().await;
            match rules.get_mut(rule_id) {
                // 只有「当前承载会话」丢失才处理，避免重挂后的旧 watcher 误触发
                Some(rule) if rule.session_id.as_deref() == Some(session_id) => {
                    rule.session_id = None;
                    rule.state = ForwardState::Reconnecting;
                    rule.error = Some("link lost, reconnecting".into());
                    Some(rule.def.clone())
                }
                _ => None,
            }
        }) else {
            return;
        };

        tracing::info!(rule = rule_id, "forward rule lost its session; recovering");
        if !self.settings().auto_restore {
            self.mark_host(
                &def.host_target(),
                ForwardState::Pending,
                Some("auto-restore disabled".into()),
            )
            .await;
            return;
        }
        self.ensure_available(&def).await;
    }

    async fn mark_host(&self, target: &str, state: ForwardState, error: Option<String>) {
        let mut rules = self.rules.lock().await;
        for rule in rules.values_mut() {
            if rule.def.host_target() == target {
                rule.state = state;
                rule.error = error.clone();
                if state == ForwardState::Pending {
                    rule.session_id = None;
                }
            }
        }
    }

    /// 重启后没有内存凭据时的兜底：主机档案里的无私钥口令 key 登录。
    async fn profile_auth(&self, target: &str) -> Option<CachedAuth> {
        let def = {
            let rules = self.rules.lock().await;
            rules
                .values()
                .find(|r| r.def.host_target() == target)?
                .def
                .clone()
        };

        let hosts = self.persist.list_hosts();
        let host = def
            .host_id
            .as_ref()
            .and_then(|id| hosts.iter().find(|h| &h.id == id).cloned())
            .or_else(|| {
                hosts.into_iter().find(|h| {
                    h.host.eq_ignore_ascii_case(&def.host)
                        && h.port == def.port
                        && h.username == def.username
                })
            })?;

        let key_path = host.key_path.filter(|p| !p.trim().is_empty())?;
        Some(CachedAuth {
            req: ConnectRequest {
                host_id: Some(host.id.clone()),
                host: def.host.clone(),
                port: def.port,
                username: def.username.clone(),
                // 只使用「无口令」的 key：口令不在磁盘上，猜不了
                auth: AuthSpec::Key {
                    key_path,
                    passphrase: None,
                },
                cols: 80,
                rows: 24,
                trust_host_key: None,
            },
            host_id: Some(host.id),
        })
    }
}

// ───────────────────────────── 工具 ─────────────────────────────

fn host_target(host: &str, port: u16, username: &str) -> String {
    format!(
        "{}:{}@{}",
        host.trim().to_ascii_lowercase(),
        port,
        username.trim()
    )
}

fn pending_info(rule: &RuleRuntime) -> ForwardInfo {
    ForwardInfo {
        id: rule.def.id.clone(),
        session_id: rule.session_id.clone().unwrap_or_default(),
        host_id: rule.def.host_id.clone(),
        host: rule.def.host.clone(),
        port: rule.def.port,
        username: rule.def.username.clone(),
        kind: rule.def.kind,
        listen_host: rule.def.listen_host.clone(),
        listen_port: rule.def.listen_port,
        remote_host: rule.def.remote_host.clone(),
        remote_port: rule.def.remote_port,
        state: rule.state,
        created_at: rule.def.created_at,
        persisted: true,
        auto_start: rule.def.auto_start,
        error: rule.error.clone(),
    }
}

/// 指数退避 + 抖动：1s, 2s, 4s … 上限 `max`。
fn backoff(attempt: u32, max: Duration) -> Duration {
    let base = 1u64 << attempt.min(6);
    let capped = Duration::from_secs(base).min(max);
    // ±20% 抖动，避免多台主机同一时刻重连
    let jitter_ms = (capped.as_millis() as u64 / 5).max(1);
    let offset = now_millis() as u64 % (jitter_ms * 2 + 1);
    capped + Duration::from_millis(offset.saturating_sub(jitter_ms))
}

/// 可重试性判定：认证 / 主机密钥问题重试只会把账号锁死，直接停。
fn is_fatal(err: &AppError) -> bool {
    match err {
        AppError::HostKey(_) => true,
        AppError::InvalidInput(_) => true,
        AppError::Ssh(msg) => {
            let m = msg.to_ascii_lowercase();
            m.contains("authentication failed")
                || m.contains("auth error")
                || m.contains("not authorized")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let max = Duration::from_secs(30);
        let a = backoff(0, max);
        assert!(a >= Duration::from_secs(1) && a < Duration::from_secs(2));
        let b = backoff(3, max);
        assert!(b >= Duration::from_secs(8) && b < Duration::from_secs(10));
        let c = backoff(20, max);
        assert!(
            c >= max && c < max + Duration::from_secs(7),
            "cap at max: {c:?}"
        );
    }

    #[test]
    fn fatal_errors_stop_retry() {
        assert!(is_fatal(&AppError::HostKey("mismatch".into())));
        assert!(is_fatal(&AppError::Ssh(
            "password authentication failed".into()
        )));
        assert!(!is_fatal(&AppError::Ssh("tcp connect timeout".into())));
        assert!(!is_fatal(&AppError::Forward("listen failed".into())));
    }

    #[test]
    fn host_target_is_case_and_space_insensitive() {
        assert_eq!(
            host_target(" Host.Example ", 22, "bob "),
            host_target("host.example", 22, "bob")
        );
    }
}
