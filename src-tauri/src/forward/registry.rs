//! 本地端口转发注册表。
//!
//! `-L` 语义：本地 TCP listener → 每条连接开一条 SSH `direct-tcpip` channel → 双向拷贝。
//!
//! 与 SSH 会话生命周期绑定：会话关闭时（`SessionEntry::closed` 的 watch 变 true），
//! 监听任务自动退出、转发置 `stopped` 并 emit `forward:state`。

use std::collections::HashMap;
use std::sync::Arc;

use tauri::{AppHandle, Emitter};
use tokio::sync::{watch, Mutex};

use crate::error::{AppError, AppResult};
use crate::forward::local;
use crate::ipc::dto::{ForwardInfo, ForwardKind, ForwardRequest, ForwardState};
use crate::ipc::events::{ForwardStateEvent, FORWARD_STATE};
use crate::ssh::manager::SessionManager;

/// 一条转发的运行期句柄。
struct ForwardEntry {
    /// 所属会话（accept 循环另用 `watch` 监听会话结束；此字段供管理面查询/过滤）
    #[allow(dead_code)]
    session_id: String,
    info: Mutex<ForwardInfo>,
    /// 通知监听任务与所有泵退出
    shutdown: watch::Sender<bool>,
}

impl ForwardEntry {
    async fn set_state(&self, state: ForwardState, error: Option<String>) {
        let mut info = self.info.lock().await;
        info.state = state;
        info.error = error;
    }

    async fn snapshot(&self) -> ForwardInfo {
        self.info.lock().await.clone()
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

    /// 启动 `-L`：监听本地端口，每条本地连接建一条 direct-tcpip channel。
    pub async fn start(&self, app: AppHandle, req: ForwardRequest) -> AppResult<ForwardInfo> {
        if req.kind != ForwardKind::Local {
            return Err(AppError::InvalidInput(
                "only local (-L) forwarding is supported in this build".into(),
            ));
        }
        if req.remote_host.trim().is_empty() {
            return Err(AppError::InvalidInput("remoteHost is required".into()));
        }
        if req.remote_port == 0 {
            return Err(AppError::InvalidInput("remotePort must not be 0".into()));
        }

        // 会话必须存在且仍在连接中（要借它的 russh handle 开 direct-tcpip）
        let session_entry = self.sessions.get(&req.session_id).await?;
        if session_entry.is_closed() {
            return Err(AppError::Ssh(format!(
                "session {} is closed",
                req.session_id
            )));
        }

        let forward_id = uuid::Uuid::new_v4().to_string();
        let listen_host = local::normalize_listen_host(&req.listen_host);
        let addr_str = local::listen_addr(&listen_host, req.listen_port)?;

        let _ = app.emit(
            FORWARD_STATE,
            ForwardStateEvent {
                forward_id: forward_id.clone(),
                state: ForwardState::Starting,
                error: None,
            },
        );

        // ── 绑定本地端口 ──
        let listener = match tokio::net::TcpListener::bind(&addr_str).await {
            Ok(l) => l,
            Err(e) => {
                let msg = format!("listen failed: {addr_str}: {e}");
                let _ = app.emit(
                    FORWARD_STATE,
                    ForwardStateEvent {
                        forward_id,
                        state: ForwardState::Failed,
                        error: Some(msg.clone()),
                    },
                );
                return Err(AppError::Forward(msg));
            }
        };
        // listen_port 传 0 时由系统分配，回填真实端口
        let actual_port = listener
            .local_addr()
            .map(|a| a.port())
            .unwrap_or(req.listen_port);

        let info = ForwardInfo {
            id: forward_id.clone(),
            session_id: req.session_id.clone(),
            kind: ForwardKind::Local,
            listen_host: listen_host.clone(),
            listen_port: actual_port,
            remote_host: req.remote_host.clone(),
            remote_port: req.remote_port,
            state: ForwardState::Running,
            error: None,
        };

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let entry = Arc::new(ForwardEntry {
            session_id: req.session_id.clone(),
            info: Mutex::new(info.clone()),
            shutdown: shutdown_tx,
        });
        self.forwards
            .lock()
            .await
            .insert(forward_id.clone(), entry.clone());

        let _ = app.emit(
            FORWARD_STATE,
            ForwardStateEvent {
                forward_id: forward_id.clone(),
                state: ForwardState::Running,
                error: None,
            },
        );

        // ── 起 accept 循环 ──
        let remote_host = req.remote_host.clone();
        let remote_port = req.remote_port as u32;
        let accept_entry = entry.clone();
        let accept_app = app.clone();
        let mut accept_stop = shutdown_rx.clone();
        let mut session_closed = session_entry.closed.subscribe();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    // 显式 stop() → 退出。Sender 被 drop 时 changed() 返回 Err，
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
                                    let guard = session_entry.handle.lock().await;
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
                                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                            }
                        }
                    }
                }
            }

            // 收尾：广播 shutdown（打断所有泵）、置状态、通知前端
            let _ = accept_entry.shutdown.send_replace(true);
            accept_entry.set_state(ForwardState::Stopped, None).await;
            let _ = accept_app.emit(
                FORWARD_STATE,
                ForwardStateEvent {
                    forward_id: forward_id,
                    state: ForwardState::Stopped,
                    error: None,
                },
            );
        });

        Ok(info)
    }

    pub async fn stop(&self, forward_id: &str) -> AppResult<()> {
        let entry = self
            .forwards
            .lock()
            .await
            .remove(forward_id)
            .ok_or_else(|| AppError::NotFound(format!("forward {forward_id}")))?;

        // 唤醒监听任务与所有泵；accept 任务退出时 emit `forward:state` = stopped
        let _ = entry.shutdown.send_replace(true);
        entry.set_state(ForwardState::Stopped, None).await;
        Ok(())
    }

    pub async fn list(&self) -> Vec<ForwardInfo> {
        let entries: Vec<Arc<ForwardEntry>> = {
            self.forwards.lock().await.values().cloned().collect()
        };
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            out.push(e.snapshot().await);
        }
        out
    }

    /// 会话关闭时停止其名下全部转发。
    ///
    /// 主路径是 accept 循环里 `session_entry.closed` 的 `watch` 自动退出；
    /// 本方法提供给显式清理场景（例如 `ssh_disconnect` 之后由上层调用）。
    #[allow(dead_code)]
    pub async fn stop_for_session(&self, session_id: &str) -> Vec<String> {
        let targets: Vec<(String, Arc<ForwardEntry>)> = {
            let map = self.forwards.lock().await;
            map.iter()
                .filter(|(_, e)| e.session_id == session_id)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };

        let mut map = self.forwards.lock().await;
        for (id, e) in &targets {
            map.remove(id);
            let _ = e.shutdown.send_replace(true);
        }
        drop(map);

        for (_, e) in &targets {
            e.set_state(ForwardState::Stopped, None).await;
        }
        targets.into_iter().map(|(id, _)| id).collect()
    }
}
