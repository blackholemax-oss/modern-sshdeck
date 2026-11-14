//! 单个 SSH 会话。
//!
//! - 持有 russh `client::Handle` 与 shell channel 写端
//! - 独立 tokio task 读 channel，输出经 base64 后 emit `terminal:data`
//! - 断开时 emit `terminal:closed` + `session:state`(closed)，并从管理器移除
//! - 独立 exec channel 支持 `run_command`（Agent 工具调用）
//! - `check_server_key` 里做 known_hosts 严格校验（见 `ssh::known_hosts`）
//! - `server_channel_open_forwarded_tcpip` 把 `-R` 来连通道交给路由表
//!
//! ## russh 0.54 关键事实（已读源码确认，勿按老教程写）
//! - `client::Handler::check_server_key` 的参数是 `&russh::keys::PublicKey`（ssh-key crate）
//! - `Channel::data()` 接收 `impl AsyncRead`，传 `&bytes[..]` 即可
//! - `ChannelMsg` 只有 `Data` / `ExtendedData` / `Eof` / `Close` / `ExitStatus` /
//!   `WindowAdjusted` / `Success` / `Failure` / `OpenFailure` 等变体
//! - `Channel::split()` 得到 `(ChannelReadHalf, ChannelWriteHalf)`
//! - `Handle::tcpip_forward` 需要 `&mut self`；`cancel_tcpip_forward` 只需 `&self`

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use base64::Engine as _;
use russh::keys::PublicKey;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, watch, Mutex};

use crate::error::{AppError, AppResult};
use crate::ipc::dto::{SessionInfo, SessionState};
use crate::ipc::events::{
    SessionStateEvent, TerminalClosedEvent, TerminalDataEvent, SESSION_STATE, TERMINAL_CLOSED,
    TERMINAL_DATA,
};
use crate::ssh::known_hosts::HostKeyPolicy;

/// shell channel 的写入命令。
#[derive(Debug)]
pub enum ChannelCommand {
    Data(Vec<u8>),
    Resize { cols: u32, rows: u32 },
    Close,
}

/// 会话写端句柄。
pub struct ShellHandle {
    pub tx: mpsc::Sender<ChannelCommand>,
}

impl ShellHandle {
    pub async fn write(&self, data: &[u8]) -> AppResult<()> {
        self.tx
            .send(ChannelCommand::Data(data.to_vec()))
            .await
            .map_err(|_| AppError::Ssh("channel closed".into()))
    }

    pub async fn resize(&self, cols: u32, rows: u32) -> AppResult<()> {
        self.tx
            .send(ChannelCommand::Resize { cols, rows })
            .await
            .map_err(|_| AppError::Ssh("channel closed".into()))
    }

    pub async fn close(&self) -> AppResult<()> {
        let _ = self.tx.send(ChannelCommand::Close).await;
        Ok(())
    }
}

// ───────────────────────────── russh handler ─────────────────────────────

/// 主机密钥校验的副产物。
///
/// `check_server_key` 返回 `false` 时，russh 只会给出一个笼统的 `UnknownKey`；
/// 具体原因（未知 / 变更 / 被吊销）要经由这个槽带回 `manager`，才能显示给用户。
#[derive(Debug, Clone, Default)]
pub struct HostKeyOutcome {
    pub fingerprint: Option<String>,
    pub algorithm: Option<String>,
    /// `verified` / `trusted` / `learned` / `insecure` / `rejected`
    pub status: Option<String>,
    /// 拒绝连接的具体原因
    pub rejection: Option<String>,
}

/// 服务端为 `-R` 远程转发打开的一条来连通道。
///
/// 由 `ClientHandler::server_channel_open_forwarded_tcpip` 生产，
/// 经 mpsc → 路由器 → 对应的转发任务。
pub struct ForwardedConn {
    pub connected_address: String,
    pub connected_port: u32,
    pub originator_address: String,
    pub originator_port: u32,
    pub channel: russh::Channel<russh::client::Msg>,
}

/// russh 客户端 handler。
///
/// 两件事：
/// 1. **主机密钥校验**：每次握手按 `HostKeyPolicy` 比对 `known_hosts`；
///    未知 / 变更 / 被吊销都会被拒绝，除非用户在前端显式确认了指纹
///    （`trusted_key`，仅对「未知」生效 —— 变更的密钥永远不放行）。
/// 2. **`-R` 来连通道**：服务端打开 `forwarded-tcpip` 通道时转交给上层路由。
pub struct ClientHandler {
    host: String,
    port: u16,
    policy: HostKeyPolicy,
    known_hosts_path: PathBuf,
    /// 用户在前端确认过的公钥；命中则写入 known_hosts 并放行。
    trusted_key: Option<PublicKey>,
    outcome: Arc<std::sync::Mutex<HostKeyOutcome>>,
    forwarded_tx: Option<mpsc::UnboundedSender<ForwardedConn>>,
}

impl ClientHandler {
    /// 返回 handler 与结果槽；连接完成后从槽里读取指纹 / 拒绝原因。
    pub fn new(
        host: String,
        port: u16,
        policy: HostKeyPolicy,
        known_hosts_path: PathBuf,
        trusted_key: Option<PublicKey>,
        forwarded_tx: Option<mpsc::UnboundedSender<ForwardedConn>>,
    ) -> (Self, Arc<std::sync::Mutex<HostKeyOutcome>>) {
        let slot = Arc::new(std::sync::Mutex::new(HostKeyOutcome::default()));
        (
            Self {
                host,
                port,
                policy,
                known_hosts_path,
                trusted_key,
                outcome: slot.clone(),
                forwarded_tx,
            },
            slot,
        )
    }

    /// 纯决策逻辑（同步 IO，文件很小）：`Ok(status)` 放行，`Err(原因)` 拒绝。
    fn decide(&self, key: &PublicKey) -> Result<&'static str, String> {
        let fp = crate::ssh::known_hosts::fingerprint(key);
        let target = crate::ssh::known_hosts::host_target(&self.host, self.port);
        let verdict = crate::ssh::known_hosts::verify_file(
            &self.known_hosts_path,
            &self.host,
            self.port,
            key,
        )
        .map_err(|e| {
            // 读不到 known_hosts 就拒绝 —— 校验失败必须是 fail-closed。
            format!(
                "cannot verify host key for {target}: {} (expected known_hosts at {}); \
                 refusing to connect with key {fp}",
                e.message(),
                self.known_hosts_path.display()
            )
        })?;

        use crate::ssh::known_hosts::{HostKeyPolicy, HostKeyVerdict};

        match verdict {
            HostKeyVerdict::Match => Ok("verified"),

            HostKeyVerdict::Revoked { line } => {
                if self.policy == HostKeyPolicy::Insecure {
                    tracing::warn!(host = %self.host, port = self.port, line, "revoked host key accepted: insecure policy");
                    return Ok("insecure");
                }
                Err(format!(
                    "host key for {target} is marked @revoked at {} line {line} (got {fp})",
                    self.known_hosts_path.display()
                ))
            }

            HostKeyVerdict::Mismatch { line } => {
                if self.policy == HostKeyPolicy::Insecure {
                    tracing::warn!(host = %self.host, port = self.port, line, "CHANGED host key accepted: insecure policy");
                    return Ok("insecure");
                }
                Err(format!(
                    "HOST KEY MISMATCH for {target}: {} line {line} records a different key \
                     (server presented {fp}). This may be a man-in-the-middle attack. \
                     Refusing to connect; remove the stale line only if you are sure the \
                     server key was legitimately replaced.",
                    self.known_hosts_path.display()
                ))
            }

            HostKeyVerdict::Unknown => {
                // ① 用户显式确认过指纹 → 落盘 + 放行
                if let Some(trusted) = &self.trusted_key {
                    if trusted.key_data() == key.key_data() {
                        return match crate::ssh::known_hosts::learn_file(
                            &self.known_hosts_path,
                            &self.host,
                            self.port,
                            key,
                        ) {
                            Ok(()) => Ok("trusted"),
                            Err(e) => Err(format!(
                                "host key for {target} was confirmed but could not be saved to {}: {}",
                                self.known_hosts_path.display(),
                                e.message()
                            )),
                        };
                    }
                    return Err(format!(
                        "pinned host key mismatch for {target}: expected {}, server presented {fp}. \
                         Refusing to connect.",
                        crate::ssh::known_hosts::fingerprint(trusted)
                    ));
                }

                // ② 策略决定未知主机怎么处理
                match self.policy {
                    HostKeyPolicy::AcceptNew => match crate::ssh::known_hosts::learn_file(
                        &self.known_hosts_path,
                        &self.host,
                        self.port,
                        key,
                    ) {
                        Ok(()) => Ok("learned"),
                        Err(e) => Err(format!(
                            "host key for {target} is unknown and could not be recorded to {}: {}",
                            self.known_hosts_path.display(),
                            e.message()
                        )),
                    },
                    HostKeyPolicy::Insecure => Ok("insecure"),
                    HostKeyPolicy::Strict => Err(format!(
                        "host key for {target} is not in {} (got {fp}). \
                         Confirm the fingerprint in the client before trusting this host.",
                        self.known_hosts_path.display()
                    )),
                }
            }
        }
    }
}

impl russh::client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        // `HashAlg::Sha256` 是 ssh-key 默认指纹算法，Display 形如 `SHA256:xxxx`。
        let fingerprint = crate::ssh::known_hosts::fingerprint(server_public_key);
        let algorithm = server_public_key.algorithm().as_str().to_string();
        let decision = self.decide(server_public_key);

        let mut guard = self
            .outcome
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.fingerprint = Some(fingerprint);
        guard.algorithm = Some(algorithm);
        match decision {
            Ok(status) => {
                guard.status = Some(status.to_string());
                guard.rejection = None;
                Ok(true)
            }
            Err(reason) => {
                tracing::warn!(reason = %reason, "host key rejected");
                guard.status = Some("rejected".into());
                guard.rejection = Some(reason);
                Ok(false)
            }
        }
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<russh::client::Msg>,
        connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        let conn = ForwardedConn {
            connected_address: connected_address.to_string(),
            connected_port,
            originator_address: originator_address.to_string(),
            originator_port,
            channel,
        };

        match self.forwarded_tx.as_ref() {
            Some(tx) => {
                if let Err(err) = tx.send(conn) {
                    // 路由已停（用户停止转发 / 会话正在收尾）：礼貌关闭通道
                    let channel = err.0.channel;
                    let _ = channel.eof().await;
                    let _ = channel.close().await;
                }
            }
            None => {
                // 从未请求过远程转发却收到来连通道：说明服务端行为异常，直接关闭。
                tracing::warn!(
                    connected_address,
                    connected_port,
                    "unexpected forwarded-tcpip channel with no remote forwarding"
                );
                let channel = conn.channel;
                let _ = channel.eof().await;
                let _ = channel.close().await;
            }
        }
        Ok(())
    }

    async fn auth_banner(
        &mut self,
        banner: &str,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        tracing::info!(banner, "ssh auth banner");
        Ok(())
    }
}

// ───────────────────────────── 会话条目 ─────────────────────────────

/// 内部会话条目。
///
/// `info` 用 `Mutex` 包裹以实现**内部可变性**：`Arc<SessionEntry>` 被多处共享时
/// 仍能更新状态（骨架里用 `Arc::get_mut` 在仍有引用时永远失败，这里改掉）。
pub struct SessionEntry {
    pub info: Mutex<SessionInfo>,
    pub shell: ShellHandle,
    /// 共享的底层 SSH 连接（多路复用，见 `ssh/link.rs`）。
    ///
    /// `run_command` 开 exec channel、端口转发开 direct-tcpip / tcpip-forward
    /// 都通过它；同一条连接上的多个会话共用同一个传输层。
    pub link: Arc<crate::ssh::link::SharedLink>,
    /// 会话结束标志（`watch`）：端口转发监听它 → 会话断开时转发自动停止。
    pub closed: watch::Sender<bool>,
    /// 常驻 receiver，保证 `closed` sender 在条目存活期间**永不进入 closed 状态**。
    ///
    /// 若没有这个 guard，sender 一旦发现「无接收者」就会让自己 closed，
    /// 之后 `subscribe()` 出来的 receiver 首次 `changed()` 会立即返回 `Err`，
    /// shell task 会瞬间误判会话已关闭并退出（表现为「连上就断」）。
    pub closed_guard: Mutex<watch::Receiver<bool>>,
    /// 隐藏会话（后台转发专用，无 PTY）：不出现在 `ssh_list_sessions` 里，
    /// 也不 emit 终端事件。
    pub hidden: bool,
    /// 建立它时使用的主机档案 id（用于转发恢复时查私钥路径等）。
    pub host_id: Option<String>,
    /// 是否已归还连接引用（幂等：close() 与 task 收尾可能同时发生）
    pub released: std::sync::atomic::AtomicBool,
}

impl SessionEntry {
    pub fn mark_closed(&self) {
        // `send_replace` 即使没有接收者也不 panic，可重复调用。
        let _ = self.closed.send_replace(true);
    }

    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    pub async fn set_state(&self, state: SessionState) {
        self.info.lock().await.state = state;
    }

    /// 会话 id（内部使用：`info` 是 Mutex，避免调用方到处锁）。
    pub async fn session_id(&self) -> String {
        self.info.lock().await.id.clone()
    }

    /// 这个会话是否还活着（会话本身与底层链路都要活着）。
    pub fn is_alive(&self) -> bool {
        !self.is_closed() && !self.link.is_closed()
    }

    /// 归还连接引用（幂等）。
    ///
    /// 两条路径都会走到：`ssh_disconnect` / `close()`，以及 shell task / 隐藏会话
    /// 监控的收尾。重复归还会让引用计数下溢，空闲回收就再也不触发了。
    pub fn release_link(&self) {
        if !self.released.swap(true, std::sync::atomic::Ordering::SeqCst) {
            self.link.release();
        }
    }
}

// ───────────────────────────── `-R` 来连通道分发 ─────────────────────────────

/// 一条远程转发注册的「收货口」。
struct RemoteRoute {
    id: u32,
    listen_host: String,
    listen_port: u32,
    tx: mpsc::UnboundedSender<ForwardedConn>,
}

/// `-R` 路由表：服务端每打开一条 `forwarded-tcpip` 通道，按
/// `(connected_address, connected_port)` 找到对应的转发任务并投递。
///
/// 匹配顺序：
/// 1. 地址与端口都一致；
/// 2. 注册时监听的是通配地址（`0.0.0.0` / `::` / 空）→ 同端口即可；
/// 3. 双方都是回环别名（`localhost` / `127.x` / `::1`）→ 同端口即可；
/// 4. 同端口只有唯一一条规则 → 直接投递（服务端回报的地址可能是本机网卡 IP）。
///
/// 都没有命中时礼貌关闭通道（而不是静默丢弃，避免服务端半开连接堆积）。
#[derive(Default)]
pub struct RemoteRouter {
    routes: Mutex<Vec<RemoteRoute>>,
    next_id: AtomicU32,
}

impl RemoteRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一条远程转发，返回用于注销的路由 id。
    pub async fn register(
        &self,
        listen_host: &str,
        listen_port: u32,
        tx: mpsc::UnboundedSender<ForwardedConn>,
    ) -> u32 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.routes.lock().await.push(RemoteRoute {
            id,
            listen_host: listen_host.to_string(),
            listen_port,
            tx,
        });
        id
    }

    pub async fn unregister(&self, route_id: u32) {
        self.routes.lock().await.retain(|r| r.id != route_id);
    }

    /// 投递一条来连通道；无路由时关闭通道。
    pub async fn dispatch(&self, conn: ForwardedConn) {
        let target = {
            let routes = self.routes.lock().await;
            pick_route(&routes, &conn.connected_address, conn.connected_port).map(|r| r.tx.clone())
        };

        match target {
            Some(tx) => {
                if let Err(err) = tx.send(conn) {
                    let channel = err.0.channel;
                    let _ = channel.eof().await;
                    let _ = channel.close().await;
                }
            }
            None => {
                tracing::debug!(
                    connected_address = %conn.connected_address,
                    connected_port = conn.connected_port,
                    "no remote forward route for incoming channel; closing"
                );
                let channel = conn.channel;
                let _ = channel.eof().await;
                let _ = channel.close().await;
            }
        }
    }
}

fn pick_route<'a>(
    routes: &'a [RemoteRoute],
    connected_address: &str,
    connected_port: u32,
) -> Option<&'a RemoteRoute> {
    // ① 地址 + 端口完全一致
    if let Some(route) = routes.iter().find(|r| {
        r.listen_port == connected_port && r.listen_host.eq_ignore_ascii_case(connected_address)
    }) {
        return Some(route);
    }
    // ② 监听通配地址
    if let Some(route) = routes.iter().find(|r| {
        r.listen_port == connected_port && is_wildcard_host(&r.listen_host)
    }) {
        return Some(route);
    }
    // ③ 回环别名
    if let Some(route) = routes.iter().find(|r| {
        r.listen_port == connected_port
            && is_loopback_host(&r.listen_host)
            && is_loopback_host(connected_address)
    }) {
        return Some(route);
    }
    // ④ 同端口唯一
    let mut same_port = routes.iter().filter(|r| r.listen_port == connected_port);
    match (same_port.next(), same_port.next()) {
        (Some(route), None) => Some(route),
        _ => None,
    }
}

fn is_wildcard_host(host: &str) -> bool {
    matches!(host.trim(), "" | "*" | "0.0.0.0" | "::" | "[::]")
}

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']').trim();
    h.eq_ignore_ascii_case("localhost")
        || h == "::1"
        || h == "0:0:0:0:0:0:0:1"
        || h.strip_prefix("127.").is_some_and(|rest| !rest.is_empty())
}

/// 起「来连通道 → 路由表」的分发任务。
///
/// 生命周期与**底层连接**一致（不是与单个会话）：同一条连接可能被多个会话
/// 共享，`-R` 规则也可能由其中任意一个会话注册，所以路由表挂在 `SharedLink` 上。
/// 链路标记 closed 或通道发送端全部 drop 时退出。
pub fn spawn_forwarded_router(
    link: Arc<crate::ssh::link::SharedLink>,
    mut rx: mpsc::UnboundedReceiver<ForwardedConn>,
) {
    tokio::spawn(async move {
        let mut closed = link.closed.subscribe();
        loop {
            tokio::select! {
                changed = closed.changed() => {
                    match changed {
                        Ok(()) => { if *closed.borrow() { break } }
                        Err(_) => break,
                    }
                }
                msg = rx.recv() => {
                    match msg {
                        Some(conn) => link.remote_routes.dispatch(conn).await,
                        None => break,
                    }
                }
            }
        }
    });
}

/// 会话表：管理器与各个 task 共享。
pub type SessionMap = Arc<Mutex<HashMap<String, Arc<SessionEntry>>>>;

fn encode_block(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

// ───────────────────────────── shell task ─────────────────────────────

/// shell 的写 / 读 task 句柄集合（放这里便于统一管理生命周期）。
pub struct ShellTasks {
    pub write: tokio::task::JoinHandle<()>,
    pub read: tokio::task::JoinHandle<()>,
}

/// 起 shell 的写 task 与读 task。
///
/// - 写 task：从 `cmd_rx` 取 `ChannelCommand` → `data` / `window_change` / `eof+close`
/// - 读 task：`channel.wait()` → `Data`/`ExtendedData` → base64 → emit `terminal:data`；
///   `Eof`/`Close`/通道耗尽 → emit `terminal:closed` 与 `session:state`(closed)，
///   并从 `sessions` 表移除
pub fn spawn_shell_tasks(
    app: AppHandle,
    session_id: String,
    channel: russh::Channel<russh::client::Msg>,
    entry: Arc<SessionEntry>,
    sessions: SessionMap,
    mut cmd_rx: mpsc::Receiver<ChannelCommand>,
) -> ShellTasks {

    // ── 单任务独占 Channel ──
    //
    // 早期实现用 `channel.split()` 起两个任务（读 task + 写 task）。那非常脆弱：
    // `split()` 之后通道的存活依赖 write half，写 task 一旦因任何原因退出就会
    // drop write half，读 task 的 `wait()` 随即返回 None，整个会话被误判为断开。
    // 这里改为**一个任务独占 Channel**，读写都在同一 select 里，生命周期单一可控。
    let task_entry = entry.clone();
    let task = tokio::spawn(async move {
        let mut closed = task_entry.closed.subscribe();
        let mut channel = channel;
        let mut reason = String::from("连接已断开");
        let mut shutdown = false;

        loop {
            tokio::select! {
                // 外部要求关闭（close()/会话被移除）→ 优雅收尾
                changed = closed.changed() => {
                    if changed.is_err() {
                        // sender 已 drop，说明条目已无引用，直接退出
                        break;
                    }
                    if *closed.borrow() {
                        shutdown = true;
                        reason = "会话已关闭".into();
                        break;
                    }
                }

                // 终端输入 / 尺寸变更 / 显式关闭
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else {
                        break;
                    };
                    match cmd {
                        ChannelCommand::Data(bytes) => {
                            if channel.data(&bytes[..]).await.is_err() {
                                reason = "写入失败".into();
                                break;
                            }
                        }
                        ChannelCommand::Resize { cols, rows } => {
                            let _ = channel.window_change(cols, rows, 0, 0).await;
                        }
                        ChannelCommand::Close => {
                            let _ = channel.eof().await;
                            let _ = channel.close().await;
                            shutdown = true;
                            reason = "会话已关闭".into();
                            break;
                        }
                    }
                }

                // 服务端输出
                msg = channel.wait() => {
                    match msg {
                        Some(russh::ChannelMsg::Data { data }) => {
                            let _ = app.emit(
                                TERMINAL_DATA,
                                TerminalDataEvent {
                                    session_id: session_id.clone(),
                                    data: encode_block(&data[..]),
                                },
                            );
                        }
                        Some(russh::ChannelMsg::ExtendedData { data, ext }) => {
                            tracing::trace!(ext, "ssh extended data");
                            let _ = app.emit(
                                TERMINAL_DATA,
                                TerminalDataEvent {
                                    session_id: session_id.clone(),
                                    data: encode_block(&data[..]),
                                },
                            );
                        }
                        Some(russh::ChannelMsg::ExitStatus { exit_status }) => {
                            if exit_status != 0 {
                                reason = format!("远程进程退出码 {exit_status}");
                            }
                        }
                        Some(russh::ChannelMsg::Eof) => {
                            reason = "远程已发送 EOF".into();
                            break;
                        }
                        Some(russh::ChannelMsg::Close) => {
                            reason = "通道已关闭".into();
                            break;
                        }
                        Some(_) => {}
                        None => {
                            break;
                        }
                    }
                }
            }
        }

        // 收尾：归还连接引用、唤醒转发监听、置状态、从会话表移除
        task_entry.release_link();
        task_entry.mark_closed();
        task_entry.set_state(SessionState::Closed).await;
        if let Some(e) = sessions.lock().await.remove(&session_id) {
            e.set_state(SessionState::Closed).await;
        }

        let _ = app.emit(
            TERMINAL_CLOSED,
            TerminalClosedEvent {
                session_id: session_id.clone(),
                reason: reason.clone(),
            },
        );
        let _ = app.emit(
            SESSION_STATE,
            SessionStateEvent {
                session_id,
                state: SessionState::Closed,
                reason: Some(reason),
            },
        );
        let _ = shutdown;
    });

    // ShellTasks 保留双句柄形状（对外契约不变），两字段指向同一个任务。
    // `read` 字段用一个永不完成的占位任务，避免同一 JoinHandle 被 await 两次。
    let read = tokio::spawn(async {
        std::future::pending::<()>().await;
    });

    ShellTasks { write: task, read }
}

// ───────────────────────────── exec channel ─────────────────────────────

/// 在**独立 exec channel** 上发起一条命令并收集 stdout+stderr（便捷入口）。
///
/// 注意：其中 `open_exec_channel` 需要 `Handle`，若调用方已有句柄锁，
/// 建议拆开调用（先 `open_exec_channel`、释放锁、再 `collect_output`），
/// 避免长时间持锁阻塞端口转发。管理器实际走的就是拆分路径。
#[allow(dead_code)]
pub async fn run_exec_command(
    handle: &russh::client::Handle<ClientHandler>,
    command: &str,
    timeout: std::time::Duration,
) -> AppResult<String> {
    let channel = open_exec_channel(handle, command).await?;
    Ok(collect_output(channel, timeout).await)
}

/// 只在句柄锁内调用：开 exec channel 并发送 exec 请求。
pub async fn open_exec_channel(
    handle: &russh::client::Handle<ClientHandler>,
    command: &str,
) -> AppResult<russh::Channel<russh::client::Msg>> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| AppError::Ssh(format!("open exec channel: {e}")))?;

    channel
        .exec(true, command.to_owned().into_bytes())
        .await
        .map_err(|e| AppError::Ssh(format!("exec request: {e}")))?;

    Ok(channel)
}

/// 读取 exec channel 的输出直到 EOF/Close 或超时（不需要 `Handle`）。
pub async fn collect_output(
    channel: russh::Channel<russh::client::Msg>,
    timeout: std::time::Duration,
) -> String {
    let mut channel = channel;
    let mut out: Vec<u8> = Vec::new();
    let mut exit_status: Option<u32> = None;
    let mut truncated = false;
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            truncated = true;
            break;
        }
        match tokio::time::timeout(remaining, channel.wait()).await {
            Ok(Some(russh::ChannelMsg::Data { data })) => out.extend_from_slice(&data[..]),
            Ok(Some(russh::ChannelMsg::ExtendedData { data, ext })) => {
                tracing::trace!(ext, "exec extended data");
                out.extend_from_slice(&data[..]);
            }
            Ok(Some(russh::ChannelMsg::ExitStatus { exit_status: code })) => {
                exit_status = Some(code);
            }
            Ok(Some(russh::ChannelMsg::Eof))
            | Ok(Some(russh::ChannelMsg::Close))
            | Ok(None) => break,
            Ok(Some(_)) => {}
            Err(_) => {
                truncated = true;
                break;
            }
        }
    }

    // 尽力收尾，失败不影响已收集输出
    let _ = channel.eof().await;
    let _ = channel.close().await;

    let mut text = String::from_utf8_lossy(&out).into_owned();
    if truncated {
        text.push_str(&format!(
            "\n[output truncated: timeout after {}s]",
            timeout.as_secs()
        ));
    } else if let Some(code) = exit_status {
        if code != 0 {
            text.push_str(&format!("\n[exit status: {code}]"));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(listen_host: &str, listen_port: u32) -> RemoteRoute {
        let (tx, _rx) = mpsc::unbounded_channel();
        RemoteRoute {
            id: listen_port,
            listen_host: listen_host.to_string(),
            listen_port,
            tx,
        }
    }

    #[test]
    fn route_matches_exact_address_first() {
        let routes = vec![route("127.0.0.1", 9000), route("10.0.0.5", 9000)];
        let hit = pick_route(&routes, "10.0.0.5", 9000).expect("exact match");
        assert_eq!(hit.listen_host, "10.0.0.5");
    }

    #[test]
    fn route_matches_wildcard_listen_address() {
        let routes = vec![route("0.0.0.0", 9000)];
        assert!(pick_route(&routes, "192.168.1.7", 9000).is_some());
        assert!(pick_route(&routes, "192.168.1.7", 9001).is_none());
    }

    #[test]
    fn route_matches_loopback_aliases() {
        let routes = vec![route("localhost", 9000)];
        assert!(pick_route(&routes, "127.0.0.1", 9000).is_some());
        assert!(pick_route(&routes, "::1", 9000).is_some());
    }

    #[test]
    fn route_falls_back_to_unique_port() {
        let routes = vec![route("10.0.0.5", 9000)];
        // 服务端回报的地址与请求不同（例如被 NAT / 网卡地址替换）
        assert!(pick_route(&routes, "203.0.113.9", 9000).is_some());

        // 同端口两条规则时不做猜测
        let ambiguous = vec![route("10.0.0.5", 9000), route("10.0.0.6", 9000)];
        assert!(pick_route(&ambiguous, "203.0.113.9", 9000).is_none());
    }

    #[test]
    fn host_helpers() {
        assert!(is_wildcard_host(""));
        assert!(is_wildcard_host("::"));
        assert!(!is_wildcard_host("127.0.0.1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("[::1]"));
        assert!(is_loopback_host("127.0.0.2"));
        assert!(!is_loopback_host("10.0.0.1"));
    }
}
