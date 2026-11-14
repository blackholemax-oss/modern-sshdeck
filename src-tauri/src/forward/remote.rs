//! 远程端口转发实现细节（`-R` 语义）。
//!
//! 方向与 `-L` 相反：
//!
//! ```text
//! 服务端监听 listen_host:listen_port ──来连──> forwarded-tcpip channel
//!      └──> 本机连 remote_host:remote_port ──> 双向泵（复用 local::bridge）
//! ```
//!
//! 与 `-L` 的关键差异是「谁在监听」：`-L` 由本机 `TcpListener` 决定何时有连接；
//! `-R` 由服务端在收到连接时**主动打开通道**，因此本模块只负责发请求 / 撤请求，
//! 来连通道由 `ssh::session::ClientHandler` 交给路由表。

use std::time::Duration;

use russh::client::Handle;
use tokio::net::TcpStream;

use crate::error::{AppError, AppResult};
use crate::ssh::session::ClientHandler;

/// 请求服务端监听；`listen_port == 0` 时返回服务端分配的端口。
pub async fn request_forward(
    handle: &mut Handle<ClientHandler>,
    listen_host: &str,
    listen_port: u16,
) -> AppResult<u16> {
    let requested = listen_port as u32;
    let allocated = handle
        .tcpip_forward(listen_host.to_string(), requested)
        .await
        .map_err(|e| {
            AppError::Forward(format!(
                "tcpip-forward {listen_host}:{listen_port} was rejected by the server: {e} \
                 (check GatewayPorts / AllowTcpForwarding)"
            ))
        })?;

    // russh 只在 port == 0 时回传服务端分配的端口，否则返回 0
    if requested == 0 {
        if allocated == 0 || allocated > u16::MAX as u32 {
            return Err(AppError::Forward(
                "server did not return a valid port for the remote forwarding request".into(),
            ));
        }
        Ok(allocated as u16)
    } else {
        Ok(listen_port)
    }
}

/// 撤销监听（停止转发 / 规则被修改时调用）。失败只记日志级别处理，由调用方决定。
pub async fn cancel_forward(
    handle: &Handle<ClientHandler>,
    listen_host: &str,
    listen_port: u16,
) -> AppResult<()> {
    handle
        .cancel_tcpip_forward(listen_host.to_string(), listen_port as u32)
        .await
        .map_err(|e| {
            AppError::Forward(format!(
                "cancel-tcpip-forward {listen_host}:{listen_port} failed: {e}"
            ))
        })
}

/// 连本机侧目标（`remote_host:remote_port`）。
///
/// 失败必须显式 close 通道，否则服务端那条 TCP 会一直挂着。
pub async fn connect_target(
    remote_host: &str,
    remote_port: u16,
    timeout: Duration,
) -> AppResult<TcpStream> {
    let host = remote_host.trim();
    if host.is_empty() {
        return Err(AppError::InvalidInput("remoteHost is required".into()));
    }
    let stream = tokio::time::timeout(timeout, TcpStream::connect((host, remote_port)))
        .await
        .map_err(|_| {
            AppError::Forward(format!(
                "connect {host}:{remote_port} timed out after {}s",
                timeout.as_secs()
            ))
        })?
        .map_err(|e| AppError::Forward(format!("connect {host}:{remote_port} failed: {e}")))?;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_target_rejects_empty_host() {
        let err = connect_target("  ", 80, Duration::from_millis(50))
            .await
            .expect_err("empty host must fail");
        assert_eq!(err.code(), "INVALID_INPUT");
    }

    #[tokio::test]
    async fn connect_target_reports_connect_failure() {
        // 127.0.0.1:1 几乎不可能有服务在听
        let err = connect_target("127.0.0.1", 1, Duration::from_millis(300))
            .await
            .expect_err("closed port must fail");
        assert_eq!(err.code(), "FORWARD_ERROR");
    }
}
