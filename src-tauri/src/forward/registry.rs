//! 端口转发注册表：规则的增删改查 + 运行期状态广播。
//!
//! 一条「规则」= 一条 `ForwardInfo` + 一个可重启的运行实例。规则的生命周期与
//! 所属 SSH 会话绑定：会话断开时规则自动停止（`state = stopped`），但**仍留在表里**，
//! 以便查询、修改（改完可重启）或删除。
//!
//! - `-L`（本地）：本机 `TcpListener` → 每条连接开 `direct-tcpip` channel
//! - `-R`（远程）：本机发起 `tcpip-forward`，服务端来连时打开 `forwarded-tcpip`
//!   channel → 本机连目标地址 → 双向泵
//!
//! ## 为什么要有 `epoch`
//!
//! `update` 的实现是「停旧实例 → 用同一 id 起新实例」。旧实例的收尾代码若直接写
//! 状态，会把新实例刚写好的 `running` 覆盖成 `stopped`（典型竞态：前端看到的
//! 规则明明是好的却显示已停止）。每次（重）启动把 `epoch` 自增，收尾时只有
//! 「自己还是当班实例」才允许写状态 / 发事件。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, watch, Mutex};

use crate::error::{AppError, AppResult};
use crate::forward::{local, remote};
use crate::ipc::dto::{ForwardInfo, ForwardKind, ForwardPatch, ForwardRequest, ForwardState};
use crate::ipc::events::{ForwardStateEvent, FORWARD_STATE};
use crate::ssh::manager::SessionManager;
use crate::ssh::session::{ForwardedConn, SessionEntry};

/// 停止 / 重启时等待旧任务退出的上限。
const STOP_TIMEOUT: Duration = Duration::from_secs(3);
/// `-R` 来连时连接本机目标的超时。
const TARGET_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// `-L` 绑定时 Windows 上端口刚释放可能短暂失败，重试几次。
const BIND_RETRIES: u32 = 5;
const BIND_RETRY_DELAY: Duration = Duration::from_millis(80);

/// 一条转发的运行期句柄。
struct ForwardEntry {
    info: Mutex<ForwardInfo>,
    /// 通知监听任务与所有泵退出；每次重启都会换一个新的 channel
    shutdown: Mutex<watch::Sender<bool>>,
    /// 运行任务（停止 / 重启时 join）
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// 当班实例代号，见模块文档
    epoch: AtomicU64,
}

impl ForwardEntry {
    fn new(info: ForwardInfo) -> Self {
        let (tx, _rx) = watch::channel(false);
        Self {
            info: Mutex::new(info),
            shutdown: Mutex::new(tx),
            task: Mutex::new(None),
            epoch: AtomicU64::new(0),
        }
    }

    async fn snapshot(&self) -> ForwardInfo {
        self.info.lock().await.clone()
    }

    async fn replace_info(&self, info: ForwardInfo) {
        *self.info.lock().await = info;
    }

    async fn update_listen_port(&self, port: u16) {
        self.info.lock().await.listen_port = port;
    }

    async fn set_state(&self, state: ForwardState, error: Option<String>) {
        let mut info = self.info.lock().await;
        info.state = state;
        info.error = error;
    }

    /// 开一次新实例：换新的 shutdown channel，返回 (epoch, 该实例的 receiver)。
    async fn begin_run(&self) -> (u64, watch::Receiver<bool>) {
        let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = watch::channel(false);
        *self.shutdown.lock().await = tx;
        (epoch, rx)
    }

    /// 让当前实例失效并把 `shutdown` 置 true。
    async fn request_stop(&self) {
        // 先让旧实例失去「写状态」的资格，再打断它
        self.epoch.fetch_add(1, Ordering::SeqCst);
        let sender = self.shutdown.lock().await.clone();
        let _ = sender.send_replace(true);
    }

    async fn join_task(&self) {
        let task = self.task.lock().await.take();
        if let Some(task) = task {
            if tokio::time::timeout(STOP_TIMEOUT, task).await.is_err() {
                tracing::debug!("forward task did not stop in time");
            }
        }
    }

    fn is_current(&self, epoch: u64) -> bool {
        self.epoch.load(Ordering::SeqCst) == epoch
    }
}

pub struct ForwardRegistry {
    forwards: Mutex<HashMap<String, Arc<ForwardEntry>>>,
    sessions: Arc<SessionManager>,
}

impl ForwardRegistry {
    pub fn new(sessions: Arc<SessionManager>) -> Self {
        Self {
            forwards: Mutex::new(HashMap::new()),
            sessions,
        }
    }

    // ───────────────────────────── 增 ─────────────────────────────

    /// 新建一条规则并立即尝试启动。
    ///
    /// 参数非法 → `Err`；运行期启动失败（端口被占用 / 服务端拒绝）→ 规则仍然
    /// 被创建，`state = failed` 且带 `error`，便于用户改参数重试或删除。
    pub async fn start(&self, app: AppHandle, req: ForwardRequest) -> AppResult<ForwardInfo> {
        let info = ForwardInfo {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: req.session_id.trim().to_string(),
            kind: req.kind,
            listen_host: local::normalize_listen_host(&req.listen_host),
            listen_port: req.listen_port,
            remote_host: req.remote_host.trim().to_string(),
            remote_port: req.remote_port,
            state: ForwardState::Starting,
            created_at: crate::persist::store::now_millis(),
            error: None,
        };
        validate(&info)?;

        let session = self.require_session(&info.session_id).await?;
        let id = info.id.clone();
        let entry = Arc::new(ForwardEntry::new(info));
        self.forwards.lock().await.insert(id, entry.clone());

        let info = self.run(app, entry, session).await;
        Ok(info)
    }

    // ───────────────────────────── 查 ─────────────────────────────

    pub async fn get(&self, forward_id: &str) -> AppResult<ForwardInfo> {
        Ok(self.entry(forward_id).await?.snapshot().await)
    }

    /// 列举规则；`session_id = Some(..)` 时只返回该会话名下的规则。
    ///
    /// 按 `created_at` 升序，保证前端列表顺序稳定（`HashMap` 顺序不保证）。
    pub async fn list(&self, session_id: Option<&str>) -> Vec<ForwardInfo> {
        let entries: Vec<Arc<ForwardEntry>> = {
            self.forwards.lock().await.values().cloned().collect()
        };
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let info = entry.snapshot().await;
            if session_id.is_none_or(|sid| info.session_id == sid) {
                out.push(info);
            }
        }
        // 同毫秒创建时用 id 兜底，保证顺序完全确定
        out.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        out
    }

    // ───────────────────────────── 改 ─────────────────────────────

    /// 修改规则：原地重启（保留 `id`）。
    ///
    /// 先停旧实例（释放本地监听 / 撤销 `tcpip-forward`），再按新参数启动。
    /// 新参数启动失败时规则保留为 `failed`，不会丢。
    pub async fn update(
        &self,
        app: AppHandle,
        forward_id: &str,
        patch: ForwardPatch,
    ) -> AppResult<ForwardInfo> {
        let entry = self.entry(forward_id).await?;
        let current = entry.snapshot().await;

        let mut next = current.clone();
        if let Some(v) = patch.kind {
            next.kind = v;
        }
        if let Some(v) = patch.session_id {
            let v = v.trim().to_string();
            if !v.is_empty() {
                next.session_id = v;
            }
        }
        if let Some(v) = patch.listen_host {
            next.listen_host = local::normalize_listen_host(&v);
        }
        if let Some(v) = patch.listen_port {
            next.listen_port = v;
        }
        if let Some(v) = patch.remote_host {
            next.remote_host = v.trim().to_string();
        }
        if let Some(v) = patch.remote_port {
            next.remote_port = v;
        }
        next.state = ForwardState::Starting;
        next.error = None;
        validate(&next)?;

        let session = self.require_session(&next.session_id).await?;

        entry.request_stop().await;
        entry.join_task().await;
        entry.replace_info(next).await;

        Ok(self.run(app, entry, session).await)
    }

    // ───────────────────────────── 删 ─────────────────────────────

    pub async fn stop(&self, app: AppHandle, forward_id: &str) -> AppResult<()> {
        let entry = self
            .forwards
            .lock()
            .await
            .remove(forward_id)
            .ok_or_else(|| AppError::NotFound(format!("forward {forward_id}")))?;

        entry.request_stop().await;
        entry.join_task().await;
        entry.set_state(ForwardState::Stopped, None).await;

        let _ = app.emit(
            FORWARD_STATE,
            ForwardStateEvent {
                forward_id: forward_id.to_string(),
                state: ForwardState::Stopped,
                error: None,
            },
        );
        Ok(())
    }

    /// 会话关闭时停止其名下全部转发（正常路径由任务自己监听 `closed` 完成，
    /// 这里用于显式清理场景）。
    #[allow(dead_code)]
    pub async fn stop_for_session(&self, session_id: &str) -> Vec<String> {
        // 先快照（持锁时不能 await），再逐条筛出同会话的
        let candidates: Vec<(String, Arc<ForwardEntry>)> = {
            self.forwards
                .lock()
                .await
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };

        let mut stopped = Vec::new();
        for (id, entry) in candidates {
            if entry.snapshot().await.session_id != session_id {
                continue;
            }
            self.forwards.lock().await.remove(&id);
            entry.request_stop().await;
            entry.join_task().await;
            entry.set_state(ForwardState::Stopped, None).await;
            stopped.push(id);
        }
        stopped
    }

    // ───────────────────────────── 内部 ─────────────────────────────

    async fn entry(&self, forward_id: &str) -> AppResult<Arc<ForwardEntry>> {
        self.forwards
            .lock()
            .await
            .get(forward_id)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("forward {forward_id}")))
    }

    async fn require_session(&self, session_id: &str) -> AppResult<Arc<SessionEntry>> {
        let entry = self.sessions.get(session_id).await?;
        if entry.is_closed() {
            return Err(AppError::Forward(format!("session {session_id} is closed")));
        }
        Ok(entry)
    }

    /// 尝试启动（或重启）一条规则，返回启动后的快照。
    ///
    /// 这里不向上抛运行期错误：失败时规则留在表里、状态为 `failed`。
    async fn run(
        &self,
        app: AppHandle,
        entry: Arc<ForwardEntry>,
        session: Arc<SessionEntry>,
    ) -> ForwardInfo {
        let info = entry.snapshot().await;
        let (epoch, shutdown_rx) = entry.begin_run().await;
        let forward_id = info.id.clone();

        emit_state(&app, &forward_id, ForwardState::Starting, None);

        match info.kind {
            ForwardKind::Local => {
                self.run_local(app, entry, session, info, epoch, shutdown_rx)
                    .await
            }
            ForwardKind::Remote => {
                self.run_remote(app, entry, session, info, epoch, shutdown_rx)
                    .await
            }
            ForwardKind::Dynamic => {
                // validate() 已经拦住了，这里只是保持 match 完备
                entry
                    .set_state(
                        ForwardState::Failed,
                        Some("dynamic forwarding is not implemented".into()),
                    )
                    .await;
                entry.snapshot().await
            }
        }
    }

    async fn run_local(
        &self,
        app: AppHandle,
        entry: Arc<ForwardEntry>,
        session: Arc<SessionEntry>,
        info: ForwardInfo,
        epoch: u64,
        shutdown_rx: watch::Receiver<bool>,
    ) -> ForwardInfo {
        let listener = match bind_listener(&info.listen_host, info.listen_port).await {
            Ok(listener) => listener,
            Err(e) => {
                entry.set_state(ForwardState::Failed, Some(e.message())).await;
                emit_state(&app, &info.id, ForwardState::Failed, Some(e.message()));
                return entry.snapshot().await;
            }
        };

        // listen_port 传 0 时由系统分配，回填真实端口
        let actual_port = listener
            .local_addr()
            .map(|a| a.port())
            .unwrap_or(info.listen_port);
        entry.update_listen_port(actual_port).await;
        entry.set_state(ForwardState::Running, None).await;
        emit_state(&app, &info.id, ForwardState::Running, None);

        let task_entry = entry.clone();
        let remote_host = info.remote_host.clone();
        let remote_port = info.remote_port as u32;
        let mut accept_stop = shutdown_rx.clone();
        let mut session_closed = session.closed.subscribe();

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    // 显式停止 → 退出。Sender 被 drop 时 changed() 返回 Err，
                    // 同样视为该退出（否则会忙等）。
                    changed = accept_stop.changed() => {
                        match changed {
                            Ok(()) => { if *accept_stop.borrow() { break } }
                            Err(_) => break,
                        }
                    }
                    // 所属 SSH 会话断开 → 转发随之停止
                    changed = session_closed.changed() => {
                        match changed {
                            Ok(()) => { if *session_closed.borrow() { break } }
                            Err(_) => break,
                        }
                    }
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((local_stream, peer)) => {
                                let open = {
                                    let guard = session.handle.lock().await;
                                    guard.channel_open_direct_tcpip(
                                        remote_host.clone(),
                                        remote_port,
                                        peer.ip().to_string(),
                                        peer.port() as u32,
                                    ).await
                                };
                                match open {
                                    Ok(channel) => {
                                        let pump_stop = shutdown_rx.clone();
                                        tokio::spawn(async move {
                                            if let Err(e) = local::bridge(
                                                local_stream, channel, pump_stop,
                                            ).await {
                                                tracing::debug!(error = %e, "forward bridge ended");
                                            }
                                        });
                                    }
                                    Err(e) => {
                                        tracing::warn!(error = %e, "direct-tcpip channel open failed");
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "forward accept failed");
                                tokio::time::sleep(Duration::from_millis(50)).await;
                            }
                        }
                    }
                }
            }

            finish_run(&task_entry, epoch, &app, ForwardState::Stopped, None).await;
        });

        *entry.task.lock().await = Some(task);
        entry.snapshot().await
    }

    async fn run_remote(
        &self,
        app: AppHandle,
        entry: Arc<ForwardEntry>,
        session: Arc<SessionEntry>,
        info: ForwardInfo,
        epoch: u64,
        shutdown_rx: watch::Receiver<bool>,
    ) -> ForwardInfo {
        // ① 让服务端开始监听（port 0 时由服务端分配）
        let actual_port = {
            let mut handle = session.handle.lock().await;
            match remote::request_forward(&mut handle, &info.listen_host, info.listen_port).await {
                Ok(port) => port,
                Err(e) => {
                    drop(handle);
                    entry.set_state(ForwardState::Failed, Some(e.message())).await;
                    emit_state(&app, &info.id, ForwardState::Failed, Some(e.message()));
                    return entry.snapshot().await;
                }
            }
        };

        entry.update_listen_port(actual_port).await;
        entry.set_state(ForwardState::Running, None).await;

        // ② 注册路由，把该地址的来连通道接到本任务
        let (conn_tx, mut conn_rx) = mpsc::unbounded_channel::<ForwardedConn>();
        let route_id = session
            .remote_routes
            .register(&info.listen_host, actual_port as u32, conn_tx)
            .await;

        emit_state(&app, &info.id, ForwardState::Running, None);

        let task_entry = entry.clone();
        let listen_host = info.listen_host.clone();
        let remote_host = info.remote_host.clone();
        let remote_port = info.remote_port;
        let mut accept_stop = shutdown_rx.clone();
        let mut session_closed = session.closed.subscribe();

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = accept_stop.changed() => {
                        match changed {
                            Ok(()) => { if *accept_stop.borrow() { break } }
                            Err(_) => break,
                        }
                    }
                    changed = session_closed.changed() => {
                        match changed {
                            Ok(()) => { if *session_closed.borrow() { break } }
                            Err(_) => break,
                        }
                    }
                    msg = conn_rx.recv() => {
                        match msg {
                            Some(conn) => {
                                let target_host = remote_host.clone();
                                let pump_stop = shutdown_rx.clone();
                                tokio::spawn(async move {
                                    let channel = conn.channel;
                                    match remote::connect_target(
                                        &target_host,
                                        remote_port,
                                        TARGET_CONNECT_TIMEOUT,
                                    ).await {
                                        Ok(stream) => {
                                            if let Err(e) = local::bridge(
                                                stream, channel, pump_stop,
                                            ).await {
                                                tracing::debug!(error = %e, "remote forward bridge ended");
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                error = %e,
                                                target = %target_host,
                                                remote_port,
                                                "remote forward target unreachable; closing channel"
                                            );
                                            let _ = channel.eof().await;
                                            let _ = channel.close().await;
                                        }
                                    }
                                });
                            }
                            // 路由表已随会话一起 drop
                            None => break,
                        }
                    }
                }
            }

            // ③ 收尾：先撤路由（后续来连会走默认关闭分支），再撤销服务端监听
            session.remote_routes.unregister(route_id).await;
            // 队列里还没处理完的来连通道要显式关闭，否则服务端那些 TCP 会半开
            while let Ok(conn) = conn_rx.try_recv() {
                let channel = conn.channel;
                let _ = channel.eof().await;
                let _ = channel.close().await;
            }
            let cancel = tokio::time::timeout(Duration::from_secs(3), session.handle.lock()).await;
            if let Ok(handle) = cancel {
                if let Err(e) = remote::cancel_forward(&handle, &listen_host, actual_port).await {
                    tracing::debug!(error = %e, "cancel-tcpip-forward failed");
                }
            } else {
                tracing::debug!("skip cancel-tcpip-forward: handle busy");
            }

            finish_run(&task_entry, epoch, &app, ForwardState::Stopped, None).await;
        });

        *entry.task.lock().await = Some(task);
        entry.snapshot().await
    }
}

/// 实例正常收尾：只有仍是当班实例才写状态、发事件。
async fn finish_run(
    entry: &ForwardEntry,
    epoch: u64,
    app: &AppHandle,
    state: ForwardState,
    error: Option<String>,
) {
    if !entry.is_current(epoch) {
        return;
    }
    entry.set_state(state, error.clone()).await;
    let info = entry.snapshot().await;
    emit_state(app, &info.id, state, error);
}

fn emit_state(app: &AppHandle, forward_id: &str, state: ForwardState, error: Option<String>) {
    let _ = app.emit(
        FORWARD_STATE,
        ForwardStateEvent {
            forward_id: forward_id.to_string(),
            state,
            error,
        },
    );
}

/// 绑定本地监听地址（带重试：Windows 上刚释放的端口偶发 `WSAEADDRINUSE`）。
async fn bind_listener(host: &str, port: u16) -> AppResult<tokio::net::TcpListener> {
    let addr = local::listen_addr(host, port)?;
    let mut last_error = String::new();
    for attempt in 0..BIND_RETRIES {
        match tokio::net::TcpListener::bind(&addr).await {
            Ok(listener) => return Ok(listener),
            Err(e) => {
                last_error = e.to_string();
                if attempt + 1 < BIND_RETRIES {
                    tokio::time::sleep(BIND_RETRY_DELAY).await;
                }
            }
        }
    }
    Err(AppError::Forward(format!(
        "listen failed: {addr}: {last_error}"
    )))
}

/// 规则参数校验（增 / 改共用）。
fn validate(info: &ForwardInfo) -> AppResult<()> {
    match info.kind {
        ForwardKind::Local | ForwardKind::Remote => {}
        ForwardKind::Dynamic => {
            return Err(AppError::InvalidInput(
                "dynamic (-D) forwarding is not implemented yet".into(),
            ))
        }
    }
    if info.session_id.trim().is_empty() {
        return Err(AppError::InvalidInput("sessionId is required".into()));
    }
    if info.remote_host.trim().is_empty() {
        return Err(AppError::InvalidInput("remoteHost is required".into()));
    }
    if info.remote_port == 0 {
        return Err(AppError::InvalidInput(
            "remotePort must be between 1 and 65535".into(),
        ));
    }
    validate_host(&info.listen_host, "listenHost")?;
    validate_host(&info.remote_host, "remoteHost")?;
    Ok(())
}

fn validate_host(host: &str, field: &str) -> AppResult<()> {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidInput(format!("{field} is required")));
    }
    if trimmed.len() > 255 || trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(AppError::InvalidInput(format!(
            "{field} contains invalid characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(kind: ForwardKind, listen_port: u16) -> ForwardInfo {
        ForwardInfo {
            id: "f1".into(),
            session_id: "s1".into(),
            kind,
            listen_host: "127.0.0.1".into(),
            listen_port,
            remote_host: "127.0.0.1".into(),
            remote_port: 80,
            state: ForwardState::Starting,
            created_at: 0,
            error: None,
        }
    }

    #[test]
    fn validate_rejects_dynamic_and_bad_ports() {
        let mut f = info(ForwardKind::Dynamic, 8080);
        assert_eq!(
            validate(&f).expect_err("dynamic must be rejected").code(),
            "INVALID_INPUT"
        );

        f = info(ForwardKind::Local, 8080);
        f.remote_port = 0;
        assert!(validate(&f).is_err(), "remote port 0 must be rejected");

        f = info(ForwardKind::Remote, 0);
        assert!(validate(&f).is_ok(), "listen port 0 is allowed (ephemeral)");

        f.remote_host = "  ".into();
        assert!(validate(&f).is_err(), "empty remote host must be rejected");

        f = info(ForwardKind::Local, 8080);
        f.listen_host = "bad host".into();
        assert!(validate(&f).is_err(), "whitespace in listen host rejected");
    }

    #[tokio::test]
    async fn bind_listener_retries_and_reports_conflict() {
        // 占住一个端口，让 bind 必然失败（并覆盖重试路径）
        let held = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = held.local_addr().unwrap().port();
        let err = bind_listener("127.0.0.1", port)
            .await
            .expect_err("port is in use");
        assert_eq!(err.code(), "FORWARD_ERROR");
    }

    #[tokio::test]
    async fn bind_listener_reports_actual_port() {
        let listener = bind_listener("127.0.0.1", 0).await.expect("bind");
        assert!(listener.local_addr().unwrap().port() > 0);
    }
}
