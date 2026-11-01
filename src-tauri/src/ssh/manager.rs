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
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tokio::sync::{watch, Mutex};

use crate::error::{AppError, AppResult};
use crate::ipc::dto::{AuthSpec, ConnectRequest, SessionInfo, SessionState};
use crate::ipc::events::{SessionStateEvent, SESSION_STATE};
use crate::ssh::auth;
use crate::ssh::config::SshOptions;
use crate::ssh::session::{
    collect_output, open_exec_channel, spawn_shell_tasks, ChannelCommand, ClientHandler,
    SessionEntry, SessionMap, ShellHandle, ShellTasks,
};

pub struct SessionManager {
    sessions: SessionMap,
    options: SshOptions,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            options: SshOptions::default(),
        }
    }

    /// 建立连接并打开交互 shell。
    ///
    /// 步骤：
    /// 1. TCP connect（受 `SshOptions::connect_timeout` 约束）
    /// 2. russh 握手；接受任意 host key，但记录 SHA256 指纹
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

        // ── 1. TCP 连接（带超时）──────────────────────────────
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

        // ── 2. russh 握手 ─────────────────────────────────────
        let mut config = russh::client::Config::default();
        config.keepalive_interval = self.options.keepalive;
        let config = Arc::new(config);

        let (handler, fp_slot) = ClientHandler::new();
        let mut handle = russh::client::connect_stream(config, stream, handler)
            .await
            .map_err(|e| AppError::Ssh(format!("ssh handshake failed: {e}")))?;

        let _ = app.emit(
            SESSION_STATE,
            SessionStateEvent {
                session_id: session_id.clone(),
                state: SessionState::Auth,
                reason: None,
            },
        );

        // ── 3. 认证 ───────────────────────────────────────────
        self.authenticate(&mut handle, &req).await?;

        // ── 4. 打开 shell channel ─────────────────────────────
        // 整个 shell 建立阶段受 `shell_timeout` 约束：认证通过后个别服务端会
        // 卡在 pty/shell 请求上，没有超时会让前端 spinner 永久转。
        let shell_result = tokio::time::timeout(self.options.shell_timeout, async {
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

        let channel = match shell_result {
            Ok(inner) => inner?,
            Err(_) => {
                return Err(AppError::Ssh(format!(
                    "open shell timed out after {}s",
                    self.options.shell_timeout.as_secs()
                )))
            }
        };

        // ── 5. 组装条目 + 起 task ─────────────────────────────
        let fingerprint = fp_slot.lock().ok().and_then(|g| g.clone());

        let info = SessionInfo {
            id: session_id.clone(),
            host: req.host.clone(),
            port: req.port,
            username: req.username.clone(),
            state: SessionState::Connected,
            created_at: crate::persist::store::now_millis(),
            host_key_fingerprint: fingerprint,
        };

        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<ChannelCommand>(64);
        // **必须保留一个 receiver**：若让它立即 drop，`closed_tx` 会进入 closed 状态，
        // 之后 `subscribe()` 得到的 receiver 首次 `changed()` 会立刻返回 Err，
        // shell task 会在启动瞬间误判为「已关闭」而退出（会话随即消失）。
        let (closed_tx, closed_rx) = watch::channel(false);
        let entry = Arc::new(SessionEntry {
            info: Mutex::new(info.clone()),
            shell: ShellHandle { tx: cmd_tx },
            handle: Mutex::new(handle),
            closed: closed_tx,
            closed_guard: Mutex::new(closed_rx),
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

        // 断开 russh 连接（失败也视为已关闭，不向上传播）。
        // 用超时包住锁 + disconnect：连接已死时 `disconnect` 可能永远等不到
        // 服务端回包，不设上限会让 `ssh_disconnect` IPC 挂住。
        let lock_result = tokio::time::timeout(Duration::from_secs(3), entry.handle.lock()).await;
        if let Ok(handle) = lock_result {
            let _ = tokio::time::timeout(
                Duration::from_secs(3),
                handle.disconnect(russh::Disconnect::ByApplication, "session closed", "en"),
            )
            .await;
        } else {
            // 拿不到锁说明有长任务正占用句柄；连接会随句柄释放而关闭。
            tracing::debug!(
                session_id,
                "skip graceful disconnect: handle busy at close time"
            );
        }

        Ok(())
    }

    pub async fn list(&self) -> Vec<SessionInfo> {
        let map = self.sessions.lock().await;
        let mut out = Vec::with_capacity(map.len());
        for e in map.values() {
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
            let handle = entry.handle.lock().await;
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
        let handle = entry.handle.lock().await;
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

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}
