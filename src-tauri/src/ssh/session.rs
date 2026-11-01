//! 单个 SSH 会话。
//!
//! - 持有 russh `client::Handle` 与 shell channel 写端
//! - 独立 tokio task 读 channel，输出经 base64 后 emit `terminal:data`
//! - 断开时 emit `terminal:closed` + `session:state`(closed)，并从管理器移除
//! - 独立 exec channel 支持 `run_command`（Agent 工具调用）
//!
//! ## russh 0.54 关键事实（已读源码确认，勿按老教程写）
//! - `client::Handler::check_server_key` 的参数是 `&russh::keys::PublicKey`（ssh-key crate）
//! - `Channel::data()` 接收 `impl AsyncRead`，传 `&bytes[..]` 即可
//! - `ChannelMsg` 只有 `Data` / `ExtendedData` / `Eof` / `Close` / `ExitStatus` /
//!   `WindowAdjusted` / `Success` / `Failure` / `OpenFailure` 等变体
//! - `Channel::split()` 得到 `(ChannelReadHalf, ChannelWriteHalf)`

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine as _;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, watch, Mutex};

use crate::error::{AppError, AppResult};
use crate::ipc::dto::{SessionInfo, SessionState};
use crate::ipc::events::{
    SessionStateEvent, TerminalClosedEvent, TerminalDataEvent, SESSION_STATE, TERMINAL_CLOSED,
    TERMINAL_DATA,
};

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

/// russh 客户端 handler。
///
/// MVP 策略：**接受任意服务端 host key**（不做 MITM 防护），但在
/// `check_server_key` 里记录 SHA256 指纹，放进 `SessionInfo::host_key_fingerprint`
/// 交给 UI，供人工比对。严格 known_hosts 校验不在 MVP 范围（见 ARCHITECTURE §1.2）。
pub struct ClientHandler {
    fingerprint: Arc<std::sync::Mutex<Option<String>>>,
}

impl ClientHandler {
    /// 返回 handler 与指纹槽；连接完成后从槽里读取指纹。
    pub fn new() -> (Self, Arc<std::sync::Mutex<Option<String>>>) {
        let slot = Arc::new(std::sync::Mutex::new(None));
        (
            Self {
                fingerprint: slot.clone(),
            },
            slot,
        )
    }
}

impl russh::client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        // `HashAlg::Sha256` 是 ssh-key 默认指纹算法，Display 形如 `SHA256:xxxx`。
        let fp = server_public_key
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string();
        if let Ok(mut guard) = self.fingerprint.lock() {
            *guard = Some(fp);
        }
        Ok(true)
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
    /// russh 连接句柄：`run_command` 开 exec channel、端口转发开 direct-tcpip 用。
    /// 只短暂持有锁，拿到的 `Channel` 可独立使用。
    pub handle: Mutex<russh::client::Handle<ClientHandler>>,
    /// 会话结束标志（`watch`）：端口转发监听它 → 会话断开时转发自动停止。
    pub closed: watch::Sender<bool>,
    /// 常驻 receiver，保证 `closed` sender 在条目存活期间**永不进入 closed 状态**。
    ///
    /// 若没有这个 guard，sender 一旦发现「无接收者」就会让自己 closed，
    /// 之后 `subscribe()` 出来的 receiver 首次 `changed()` 会立即返回 `Err`，
    /// shell task 会瞬间误判会话已关闭并退出（表现为「连上就断」）。
    pub closed_guard: Mutex<watch::Receiver<bool>>,
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

        // 收尾：唤醒转发监听、置状态、从会话表移除
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
