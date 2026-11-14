//! SSH 会话管理器。
//!
//! `SessionManager` 是所有会话的唯一入口。内部持有 `HashMap<String, Arc<SessionEntry>>`。
//!
//! 生命周期（ARCHITECTURE §4.3）：
//!
//! ```text
//! Idle ──connect()──> Authenticating ──ok──> Connected ──open_shell──> Shell
//!                    │           │
//!                    └──fail────> Failed        └──disconnect()──> Closed
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use russh::keys::PublicKey;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, watch, Mutex};

use crate::error::{AppError, AppResult};
use crate::ipc::dto::{
    AuthSpec, ConnectRequest, HostKeyProbe, PoolStatsDto, SessionInfo, SessionState,
};
use crate::ipc::events::{SessionStateEvent, SESSION_STATE};
use crate::ssh::auth;
use crate::ssh::config::SshOptions;
use crate::ssh::known_hosts::{self, HostKeyPolicy};
use crate::ssh::link::{link_key, LinkPool, SharedLink};
use crate::ssh::session::{
    collect_output, open_exec_channel, spawn_forwarded_router, spawn_shell_tasks, ChannelCommand,
    ClientHandler, ForwardedConn, SessionEntry, SessionMap, ShellHandle, ShellTasks,
};

/// 主机密钥校验设置（可随配置热更新）。
#[derive(Debug, Clone)]
pub struct HostKeySettings {
    pub policy: HostKeyPolicy,
    pub known_hosts_path: PathBuf,
}

impl Default for HostKeySettings {
    fn default() -> Self {
        Self {
            policy: HostKeyPolicy::default(),
            known_hosts_path: known_hosts::default_path(),
        }
    }
}

impl HostKeySettings {
    /// 从配置值构造：策略非法值回落到默认（strict），空路径用默认位置。
    pub fn from_config(policy: &str, path: &str) -> Self {
        let trimmed = path.trim();
        Self {
            policy: HostKeyPolicy::parse(policy).unwrap_or_default(),
            known_hosts_path: if trimmed.is_empty() {
                known_hosts::default_path()
            } else {
                auth::expand_path(trimmed)
            },
        }
    }
}

/// 连接复用设置。
#[derive(Debug, Clone, Copy)]
pub struct ReuseSettings {
    pub enabled: bool,
    /// 引用归零后连接保留多久（0 = 立刻断开）
    pub idle_ttl: Duration,
    /// 建立连接的最大等待（含 TCP + 握手 + 认证）
    pub connect_timeout: Duration,
}

impl Default for ReuseSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_ttl: Duration::from_secs(300),
            connect_timeout: Duration::from_secs(30),
        }
    }
}

pub struct SessionManager {
    sessions: SessionMap,
    options: SshOptions,
    /// `RwLock` 而非 Mutex：连接建立时高频读取，写只在改设置时发生。
    host_key: std::sync::RwLock<HostKeySettings>,
    /// 连接池：同身份（host+port+user+凭据）的多路会话共享一条 SSH 传输。
    links: Arc<LinkPool>,
    reuse: std::sync::RwLock<ReuseSettings>,
    /// 同一身份的并发建连串行化，避免同时 miss 后握两次手。
    connect_gates: std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            options: SshOptions::default(),
            host_key: std::sync::RwLock::new(HostKeySettings::default()),
            links: Arc::new(LinkPool::new()),
            reuse: std::sync::RwLock::new(ReuseSettings::default()),
            connect_gates: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn links(&self) -> Arc<LinkPool> {
        self.links.clone()
    }

    pub fn reuse_settings(&self) -> ReuseSettings {
        match self.reuse.read() {
            Ok(g) => *g,
            Err(p) => *p.into_inner(),
        }
    }

    /// 应用「连接复用」设置；关闭复用时立刻断开所有空闲连接。
    pub fn apply_reuse_settings(&self, enabled: bool, idle_secs: u32) {
        let ttl = Duration::from_secs(idle_secs.min(24 * 3600) as u64);
        {
            let mut guard = match self.reuse.write() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            guard.enabled = enabled;
            guard.idle_ttl = ttl;
        }
        self.links.set_idle_ttl(ttl);
        if !enabled {
            let pool = self.links.clone();
            // 用 tauri 的运行时：本函数会在 `setup`（没有 tokio reactor 上下文）里被调用，
            // 直接 `tokio::spawn` 会 panic "there is no reactor running"。
            tauri::async_runtime::spawn(async move {
                pool.close_idle_now().await;
            });
        }
        tracing::info!(enabled, idle_secs, "ssh link reuse settings updated");
    }

    /// 后台回收空闲连接（由 `lib.rs` 在 setup 里起一次）。
    pub fn spawn_link_reaper(&self) {
        let pool = self.links.clone();
        // 同上：启动期调用，必须走 tauri 的运行时
        tauri::async_runtime::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(10));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let reaped = pool.reap_idle().await;
                if !reaped.is_empty() {
                    tracing::info!(count = reaped.len(), "closed idle ssh links");
                }
            }
        });
    }

    pub async fn pool_stats(&self) -> PoolStatsDto {
        let (links, handshakes, reuses) = self.links.stats().await;
        let sessions = self
            .sessions
            .lock()
            .await
            .values()
            .filter(|s| !s.hidden)
            .count();
        PoolStatsDto {
            links,
            handshakes,
            reuses,
            sessions,
        }
    }

    /// 同一身份的建连串行化锁。
    async fn connect_gate(&self, key: &str) -> Arc<Mutex<()>> {
        let mut gates = match self.connect_gates.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        gates
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// 应用设置面板里的主机密钥策略 / known_hosts 路径（只影响之后建立的连接）。
    pub fn apply_host_key_settings(&self, policy: &str, known_hosts_path: &str) {
        let next = HostKeySettings::from_config(policy, known_hosts_path);
        tracing::info!(
            policy = next.policy.as_str(),
            path = %next.known_hosts_path.display(),
            "host key policy updated"
        );
        match self.host_key.write() {
            Ok(mut guard) => *guard = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }

    pub fn host_key_settings(&self) -> HostKeySettings {
        match self.host_key.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// 建立连接并打开交互 shell。
    ///
    /// 步骤：
    /// 1. TCP connect（受 `SshOptions::connect_timeout` 约束）
    /// 2. russh 握手；按 `HostKeyPolicy` 严格比对 `known_hosts`
    /// 3. 认证（密码 / 私钥，私钥支持 passphrase）
    /// 4. `channel_open_session` → `request_pty` → `request_shell`
    /// 5. 起读/写 task；emit `session:state` = connected
    pub async fn create(&self, app: AppHandle, req: ConnectRequest) -> AppResult<SessionInfo> {
        if req.port == 0 {
            return Err(AppError::InvalidInput("port must not be 0".into()));
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        let _ = app.emit(
            SESSION_STATE,
            SessionStateEvent {
                session_id: session_id.clone(),
                state: SessionState::Connecting,
                reason: None,
            },
        );

        // 失败时统一 emit `session:state` = failed，前端据此把标签置红。
        let result = self.create_inner(app.clone(), &session_id, req).await;
        if let Err(e) = &result {
            let _ = app.emit(
                SESSION_STATE,
                SessionStateEvent {
                    session_id,
                    state: SessionState::Failed,
                    reason: Some(e.message()),
                },
            );
        }
        result
    }

    async fn create_inner(
        &self,
        app: AppHandle,
        session_id: &str,
        req: ConnectRequest,
    ) -> AppResult<SessionInfo> {
        let session_id = session_id.to_string();

        // ── 1. 取连接：命中连接池就跳过 TCP + 握手 + 认证 ────────
        let link = self.obtain_link(&req).await?;

        let _ = app.emit(
            SESSION_STATE,
            SessionStateEvent {
                session_id: session_id.clone(),
                state: SessionState::Auth,
                reason: None,
            },
        );

        // ── 2. 在共享连接上开一个独立 shell channel ─────────────
        let channel = match self.open_shell_on(&link, &req).await {
            Ok(channel) => channel,
            Err(e) => {
                // 建连失败要把引用还回去，否则空闲连接永远回收不掉
                link.release();
                return Err(e);
            }
        };

        // ── 3. 组装条目 + 起 task ─────────────────────────────
        let info = SessionInfo {
            id: session_id.clone(),
            host: req.host.clone(),
            port: req.port,
            username: req.username.clone(),
            state: SessionState::Connected,
            created_at: crate::persist::store::now_millis(),
            host_key_fingerprint: link.host_key.fingerprint.clone(),
            host_key_status: link.host_key.status.clone(),
            link_id: Some(link.id.clone()),
        };

        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<ChannelCommand>(64);
        // **必须保留一个 receiver**：若让它立即 drop，`closed_tx` 会进入 closed 状态，
        // 之后 `subscribe()` 得到的 receiver 首次 `changed()` 会立刻返回 Err，
        // shell task 会在启动瞬间误判为「已关闭」而退出（会话随即消失）。
        let (closed_tx, closed_rx) = watch::channel(false);
        let entry = Arc::new(SessionEntry {
            info: Mutex::new(info.clone()),
            shell: ShellHandle { tx: cmd_tx },
            link: link.clone(),
            closed: closed_tx,
            closed_guard: Mutex::new(closed_rx),
            hidden: false,
            host_id: req.host_id.clone(),
            released: std::sync::atomic::AtomicBool::new(false),
        });

        // **先入表再起 task**：读任务在通道结束时会把自身从表中移除，
        // 若先起 task 后入表，快速断开的会话可能出现「先删后插」的残留条目。
        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), entry.clone());

        let tasks: ShellTasks = spawn_shell_tasks(
            app.clone(),
            session_id.clone(),
            channel,
            entry.clone(),
            self.sessions.clone(),
            cmd_rx,
        );
        // task 自身持有 channel；句柄只为可观测性保留（Drop 时不取消）。
        let _ = (tasks.write, tasks.read);

        let _ = app.emit(
            SESSION_STATE,
            SessionStateEvent {
                session_id,
                state: SessionState::Connected,
                reason: None,
            },
        );

        Ok(info)
    }

    // ───────────────────── 连接复用（共享底层连接）─────────────────────

    /// 取一条可用的底层连接：命中池就复用（0 次握手），否则新建。
    ///
    /// 同一身份用 per-key 串行锁保护，避免并发登录时重复握手。
    async fn obtain_link(&self, req: &ConnectRequest) -> AppResult<Arc<SharedLink>> {
        let settings = self.reuse_settings();
        let key = link_key(&req.host, req.port, &req.username, &req.auth);

        if settings.enabled {
            if let Some(link) = self.links.find(&key).await {
                link.acquire();
                tracing::info!(
                    link = %link.id,
                    host = %req.host,
                    port = req.port,
                    user = %req.username,
                    "reuse existing ssh link (handshake skipped)"
                );
                return Ok(link);
            }
        }

        let gate = self.connect_gate(&key).await;
        let _guard = gate.lock().await;

        // 双检：等锁期间可能已经有人建好同一条连接
        if settings.enabled {
            if let Some(link) = self.links.find(&key).await {
                link.acquire();
                return Ok(link);
            }
        }

        let link = self.establish_link(req, key).await?;
        link.acquire();
        Ok(link)
    }

    /// 新建一条底层连接（TCP + 握手 + 认证），放进池里并起监控任务。
    async fn establish_link(
        &self,
        req: &ConnectRequest,
        key: String,
    ) -> AppResult<Arc<SharedLink>> {
        let addr = format!("{}:{}", req.host, req.port);
        let stream = tokio::time::timeout(
            self.options.connect_timeout,
            tokio::net::TcpStream::connect(&addr),
        )
        .await
        .map_err(|_| {
            AppError::Ssh(format!(
                "tcp connect {addr} timed out after {}s",
                self.options.connect_timeout.as_secs()
            ))
        })?
        .map_err(|e| AppError::Ssh(format!("tcp connect {addr} failed: {e}")))?;

        let _ = stream.set_nodelay(true);

        let mut config = russh::client::Config::default();
        config.keepalive_interval = self.options.keepalive;
        let config = Arc::new(config);

        let host_key_settings = self.host_key_settings();
        // 用户在前端确认过的公钥：只对「未知」有效，「变更」永远拒绝。
        let trusted_key = match req.trust_host_key.as_deref().map(str::trim) {
            Some(raw) if !raw.is_empty() => Some(PublicKey::from_openssh(raw).map_err(|e| {
                AppError::InvalidInput(format!("trustHostKey is not a valid OpenSSH public key: {e}"))
            })?),
            _ => None,
        };

        // `-R` 来连通道：handler 生产、链路路由器消费
        let (forwarded_tx, forwarded_rx) = mpsc::unbounded_channel::<ForwardedConn>();

        let (handler, outcome_slot) = ClientHandler::new(
            req.host.clone(),
            req.port,
            host_key_settings.policy,
            host_key_settings.known_hosts_path.clone(),
            trusted_key,
            Some(forwarded_tx),
        );

        let handshake = tokio::time::timeout(
            self.options.connect_timeout,
            russh::client::connect_stream(config, stream, handler),
        )
        .await
        .map_err(|_| {
            AppError::Ssh(format!(
                "ssh handshake with {addr} timed out after {}s",
                self.options.connect_timeout.as_secs()
            ))
        })?;

        let mut handle = match handshake {
            Ok(handle) => handle,
            Err(e) => {
                // `check_server_key` 返回 false 时 russh 只会报 `UnknownKey`，
                // 真正的原因（未知 / 变更 / 被吊销 / 文件读失败）在结果槽里。
                let rejection = outcome_slot
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .rejection
                    .clone();
                return Err(match rejection {
                    Some(reason) => AppError::HostKey(reason),
                    None => AppError::Ssh(format!("ssh handshake failed: {e}")),
                });
            }
        };

        self.authenticate(&mut handle, req).await?;

        let outcome = outcome_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();

        let link = Arc::new(SharedLink::new(
            uuid::Uuid::new_v4().to_string(),
            key,
            req.host.trim().to_string(),
            req.port,
            req.username.clone(),
            handle,
            outcome,
            crate::persist::store::now_millis(),
        ));

        self.links.insert(link.clone()).await;
        spawn_forwarded_router(link.clone(), forwarded_rx);
        spawn_link_monitor(link.clone(), self.links.clone());

        tracing::info!(
            link = %link.id,
            host = %req.host,
            port = req.port,
            user = %req.username,
            "ssh link established"
        );
        Ok(link)
    }

    /// 在共享连接上开一个 shell channel（PTY + shell）。
    async fn open_shell_on(
        &self,
        link: &Arc<SharedLink>,
        req: &ConnectRequest,
    ) -> AppResult<russh::Channel<russh::client::Msg>> {
        // 整个建立阶段受 `shell_timeout` 约束：个别服务端会卡在 pty/shell
        // 请求上，没有超时会让前端 spinner 永久转。
        let shell_result = tokio::time::timeout(self.options.shell_timeout, async {
            let handle = link.handle.lock().await;
            let channel = handle
                .channel_open_session()
                .await
                .map_err(|e| AppError::Ssh(format!("open session channel: {e}")))?;

            // 终端模式：与 OpenSSH 客户端一致的基础集合
            // （UTF-8、回车/换行、回显、常用控制键）。缺失会导致退格、
            // Ctrl-C、中文输入异常。
            //
            // **绝不能把 `TTY_OP_END` 放进这个数组。** 它是 RFC4254 中终止
            // mode 列表的哨兵值，应由协议编码层自行追加；若作为真实 mode 传入，
            // 编码出的列表会提前终止，剩余字节被服务端当成后续字段，解析错位后
            // 不回 `pty-req` 的 reply —— `request_pty(want_reply = true)` 就永久
            // 挂起，最终被 `shell_timeout` 打断。外部表现是「连上却没有提示符，
            // 会话随即消失」。
            let terminal_modes: Vec<(russh::Pty, u32)> = vec![
                (russh::Pty::VINTR, 3),
                (russh::Pty::VQUIT, 28),
                (russh::Pty::VERASE, 127),
                (russh::Pty::VKILL, 21),
                (russh::Pty::VEOF, 4),
                (russh::Pty::ICRNL, 1),
                (russh::Pty::IXON, 1),
                (russh::Pty::IUTF8, 1),
                (russh::Pty::ISIG, 1),
                (russh::Pty::ICANON, 1),
                (russh::Pty::ECHO, 1),
                (russh::Pty::ECHOE, 1),
                (russh::Pty::ECHOK, 1),
                (russh::Pty::ECHOCTL, 1),
                (russh::Pty::OPOST, 1),
            ];

            channel
                .request_pty(
                    true,
                    SshOptions::TERM,
                    req.cols.max(1),
                    req.rows.max(1),
                    0,
                    0,
                    &terminal_modes,
                )
                .await
                .map_err(|e| AppError::Ssh(format!("request pty: {e}")))?;

            channel
                .request_shell(true)
                .await
                .map_err(|e| AppError::Ssh(format!("request shell: {e}")))?;

            Ok::<_, AppError>(channel)
        })
        .await;

        match shell_result {
            Ok(inner) => inner,
            Err(_) => Err(AppError::Ssh(format!(
                "open shell timed out after {}s",
                self.options.shell_timeout.as_secs()
            ))),
        }
    }

    /// 建立一条**无终端**的后台会话，专供端口转发守护使用。
    ///
    /// 它同样走连接池：如果用户已经在这个主机上有活着的会话，直接复用同一条
    /// 底层连接（0 次握手）；反之它会把连接留住，让用户后续打开终端时复用。
    /// 隐藏会话不出现在 `ssh_list_sessions` 里，也不 emit 终端事件。
    pub async fn connect_headless(
        &self,
        req: ConnectRequest,
        host_id: Option<String>,
    ) -> AppResult<Arc<SessionEntry>> {
        if req.port == 0 {
            return Err(AppError::InvalidInput("port must not be 0".into()));
        }
        let session_id = uuid::Uuid::new_v4().to_string();
        let link = self.obtain_link(&req).await?;

        let info = SessionInfo {
            id: session_id.clone(),
            host: req.host.clone(),
            port: req.port,
            username: req.username.clone(),
            state: SessionState::Connected,
            created_at: crate::persist::store::now_millis(),
            host_key_fingerprint: link.host_key.fingerprint.clone(),
            host_key_status: link.host_key.status.clone(),
            link_id: Some(link.id.clone()),
        };

        // 接收端立刻丢弃：隐藏会话没有终端输入，写入会返回「channel closed」。
        let (cmd_tx, _cmd_rx) = mpsc::channel::<ChannelCommand>(1);
        let (closed_tx, closed_rx) = watch::channel(false);
        let entry = Arc::new(SessionEntry {
            info: Mutex::new(info),
            shell: ShellHandle { tx: cmd_tx },
            link: link.clone(),
            closed: closed_tx,
            closed_guard: Mutex::new(closed_rx),
            hidden: true,
            host_id,
            released: std::sync::atomic::AtomicBool::new(false),
        });

        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), entry.clone());

        // 链路断开 → 标记会话关闭并出表（供转发任务与监督器感知）
        let watcher_entry = entry.clone();
        let sessions = self.sessions.clone();
        let sid = session_id.clone();
        tokio::spawn(async move {
            let mut closed = watcher_entry.link.closed.subscribe();
            loop {
                match closed.changed().await {
                    Ok(()) => {
                        if *closed.borrow() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            watcher_entry.release_link();
            watcher_entry.mark_closed();
            watcher_entry.set_state(SessionState::Closed).await;
            sessions.lock().await.remove(&sid);
        });

        Ok(entry)
    }

    /// 认证：密码 / 私钥（自动处理 RSA hash 算法与 passphrase）。
    async fn authenticate(
        &self,
        handle: &mut russh::client::Handle<ClientHandler>,
        req: &ConnectRequest,
    ) -> AppResult<()> {
        match &req.auth {
            AuthSpec::Password { password } => {
                let result = handle
                    .authenticate_password(req.username.clone(), password.clone())
                    .await
                    .map_err(|e| AppError::Ssh(format!("password auth error: {e}")))?;
                if !result.success() {
                    return Err(AppError::Ssh(
                        "password authentication failed (check username/password)".into(),
                    ));
                }
            }
            AuthSpec::Key { key_path, passphrase } => {
                let key = auth::load_private_key(key_path, passphrase.as_deref())?;

                // RSA 需要协商出服务端支持的签名 hash 算法（rsa-sha2-256/512），
                // 否则部分服务端会拒绝 legacy ssh-rsa。
                let hash_alg = if key.algorithm().is_rsa() {
                    match handle.best_supported_rsa_hash().await {
                        Ok(Some(best)) => best,
                        // 服务端未声明 server-sig-algs 时退回 SHA-256 尝试
                        Ok(None) => Some(russh::keys::HashAlg::Sha256),
                        Err(_) => Some(russh::keys::HashAlg::Sha256),
                    }
                } else {
                    None
                };

                let wrapped = auth::with_hash_alg(key, hash_alg);
                let result = handle
                    .authenticate_publickey(req.username.clone(), wrapped)
                    .await
                    .map_err(|e| AppError::Ssh(format!("publickey auth error: {e}")))?;
                if !result.success() {
                    return Err(AppError::Ssh(format!(
                        "publickey authentication failed (key {} may not be authorized)",
                        auth::expand_path(key_path).display()
                    )));
                }
            }
        }
        Ok(())
    }

    pub async fn close(&self, session_id: &str) -> AppResult<()> {
        let entry = self
            .sessions
            .lock()
            .await
            .remove(session_id)
            .ok_or_else(|| AppError::NotFound(format!("session {session_id}")))?;

        // 优雅关闭：先让 shell 发 EOF/close（远程 shell 正常退出），
        // 再标记 closed 结束读/写 task，最后断开 russh 连接。
        let _ = entry.shell.close().await;
        // 给读 task 一点时间处理通道关闭并 emit `terminal:closed`
        tokio::time::sleep(Duration::from_millis(150)).await;
        entry.mark_closed();
        entry.set_state(SessionState::Closed).await;

        // 归还连接引用：复用开启时连接留在池里（空闲 TTL 内可零握手重连），
        // 关闭复用时立刻断开，保持「一个会话一条连接」的旧语义。
        entry.release_link();
        if !self.reuse_settings().enabled {
            let key = entry.link.key.clone();
            if entry.link.refs.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                if let Some(link) = self.links.remove(&key).await {
                    LinkPool::close_link(&link, "session closed").await;
                }
            }
        }

        Ok(())
    }

    pub async fn list(&self) -> Vec<SessionInfo> {
        let entries: Vec<Arc<SessionEntry>> = {
            let map = self.sessions.lock().await;
            map.values().filter(|e| !e.hidden).cloned().collect()
        };
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            out.push(e.info.lock().await.clone());
        }
        out
    }

    pub async fn get(&self, session_id: &str) -> AppResult<Arc<SessionEntry>> {
        self.sessions
            .lock()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("session {session_id}")))
    }

    /// 找出该主机下还活着的会话（优先用户可见的终端会话，其次后台转发会话）。
    ///
    /// 转发守护用它把规则挂到已有连接上，避免为恢复转发再握一次手。
    pub async fn find_live(
        &self,
        host: &str,
        port: u16,
        username: &str,
    ) -> Option<Arc<SessionEntry>> {
        let entries: Vec<Arc<SessionEntry>> = {
            let map = self.sessions.lock().await;
            map.values().cloned().collect()
        };

        let mut hidden_hit: Option<Arc<SessionEntry>> = None;
        for entry in entries {
            let matched = {
                let info = entry.info.lock().await;
                info.host.eq_ignore_ascii_case(host.trim())
                    && info.port == port
                    && info.username == username
            };
            if !matched || entry.is_closed() || entry.link.is_closed() {
                continue;
            }
            if !entry.hidden {
                return Some(entry);
            }
            if hidden_hit.is_none() {
                hidden_hit = Some(entry);
            }
        }
        hidden_hit
    }

    pub async fn write_input(&self, session_id: &str, data: &[u8]) -> AppResult<()> {
        let entry = self.get(session_id).await?;
        if entry.is_closed() {
            return Err(AppError::Ssh(format!("session {session_id} is closed")));
        }
        entry.shell.write(data).await
    }

    pub async fn resize(&self, session_id: &str, cols: u32, rows: u32) -> AppResult<()> {
        let entry = self.get(session_id).await?;
        entry.shell.resize(cols, rows).await
    }

    /// 只做握手、不做认证，取回服务端出示的主机公钥。
    ///
    /// 用途：strict 策略下首次连接会被拒绝，前端拿到指纹让用户确认后再带
    /// `trustHostKey` 重连 —— 这样「信任」永远是用户看过指纹后显式做出的动作，
    /// 而不是登录失败后盲目重试。
    pub async fn probe_host_key(&self, host: &str, port: u16) -> AppResult<HostKeyProbe> {
        let host = host.trim();
        if host.is_empty() {
            return Err(AppError::InvalidInput("host is required".into()));
        }
        if port == 0 {
            return Err(AppError::InvalidInput("port must not be 0".into()));
        }

        let addr = format!("{host}:{port}");
        let stream = tokio::time::timeout(
            self.options.connect_timeout,
            tokio::net::TcpStream::connect(&addr),
        )
        .await
        .map_err(|_| AppError::Ssh(format!("tcp connect {addr} timed out")))?
        .map_err(|e| AppError::Ssh(format!("tcp connect {addr} failed: {e}")))?;

        let config = Arc::new(russh::client::Config::default());
        let slot: Arc<std::sync::Mutex<Option<PublicKey>>> = Arc::new(std::sync::Mutex::new(None));
        let handler = ProbeHandler { key: slot.clone() };

        let handle = tokio::time::timeout(
            self.options.connect_timeout,
            russh::client::connect_stream(config, stream, handler),
        )
        .await
        .map_err(|_| AppError::Ssh(format!("ssh handshake with {addr} timed out")))?
        .map_err(|e| AppError::Ssh(format!("ssh handshake with {addr} failed: {e}")))?;

        let key = slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or_else(|| AppError::Ssh(format!("{addr} did not present a host key")))?;

        // 握手已完成，主动断开，避免把只用于取指纹的连接留在服务端
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            handle.disconnect(russh::Disconnect::ByApplication, "host key probe", "en"),
        )
        .await;

        let settings = self.host_key_settings();
        let verdict = known_hosts::verify_file(&settings.known_hosts_path, host, port, &key)?;
        let public_key = key
            .to_openssh()
            .map_err(|e| AppError::Ssh(format!("encode host key failed: {e}")))?;

        Ok(HostKeyProbe {
            host: host.to_string(),
            port,
            fingerprint: known_hosts::fingerprint(&key),
            algorithm: key.algorithm().as_str().to_string(),
            public_key,
            known_hosts_path: settings.known_hosts_path.display().to_string(),
            policy: settings.policy.as_str().to_string(),
            status: verdict.status().to_string(),
        })
    }

    /// Agent 工具调用：在同一 SSH 连接上开**独立 exec channel** 执行命令。
    ///
    /// - 不影响交互 shell（不同 channel）
    /// - 收集 stdout+stderr；超时返回已收集部分并标注截断
    pub async fn run_command(
        &self,
        session_id: &str,
        command: &str,
        timeout: Duration,
    ) -> AppResult<String> {
        let entry = self.get(session_id).await?;
        if command.trim().is_empty() {
            return Err(AppError::InvalidInput("command is required".into()));
        }

        // 只在开 channel / 发 exec 请求时短暂持有句柄锁；
        // 读取输出阶段不持锁，避免阻塞端口转发开新 channel。
        let channel = {
            let handle = entry.link.handle.lock().await;
            open_exec_channel(&handle, command).await?
        };

        Ok(collect_output(channel, timeout).await)
    }

    /// 打开一个 SFTP 会话（每次操作独立开 channel）。
    ///
    /// 为什么不常驻一个 SftpSession：
    /// - 它绑定在 channel 上，长时间空闲会被服务端断开；
    /// - 把它放进 `SessionEntry` 要给 Mutex 加一层生命周期管理，
    ///   而文件操作是低频交互，重开 channel 的代价可以接受。
    pub async fn open_sftp(
        &self,
        session_id: &str,
    ) -> AppResult<russh_sftp::client::SftpSession> {
        let entry = self.get(session_id).await?;
        let handle = entry.link.handle.lock().await;
        crate::ssh::sftp::open_sftp(&handle).await
    }

    /// 标记会话状态并广播（内部使用）。
    ///
    /// 注：事件 `session:state` 的 emit 需要 `AppHandle`，由 `create` / 读任务负责；
    /// 本方法只做状态落库，供关闭流程与外部查询使用。
    #[allow(dead_code)]
    pub async fn set_state(&self, session_id: &str, state: SessionState, reason: Option<String>) {
        let entry = self.sessions.lock().await.get(session_id).cloned();
        let Some(entry) = entry else { return };
        entry.set_state(state).await;
        // 事件广播需要 AppHandle，这里只做状态落库；事件由调用方 emit。
        let _ = reason;
    }
}

/// 监控一条底层连接：对端关闭 / 网络中断时标记 closed 并移出池。
///
/// 各个共享会话自己会在 `channel.wait()` 返回 `None` 时收尾；这里负责让
/// 「连接池状态」与「路由器 / 回收器」也立刻感知，避免把死连接当成可复用的。
fn spawn_link_monitor(link: Arc<SharedLink>, pool: Arc<LinkPool>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if link.is_closed() {
                break;
            }
            if link.handle_is_closed() {
                tracing::warn!(link = %link.id, host = %link.host, "ssh link closed by peer");
                link.mark_closed();
                pool.remove(&link.key).await;
                break;
            }
        }
    });
}

/// 只用于「取主机公钥」的探针 handler：记录密钥后放行，不做认证。
struct ProbeHandler {
    key: Arc<std::sync::Mutex<Option<PublicKey>>>,
}

impl russh::client::Handler for ProbeHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        *self
            .key
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(server_public_key.clone());
        Ok(true)
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}
