//! 本地转发实现细节（`-L` 语义）。
//!
//! 本模块放置可独立测试的辅助函数：监听地址规范化、端口可用性检查、
//! 以及单条本地连接 ↔ SSH channel 的双向泵（`bridge`）。

use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::{AppError, AppResult};

/// 默认监听地址：仅本机回环，避免把转发端口暴露到局域网。
pub fn default_listen_host() -> &'static str {
    "127.0.0.1"
}

/// 规范化监听地址：空串 / 缺省时回落到 `127.0.0.1`。
pub fn normalize_listen_host(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        default_listen_host().to_string()
    } else {
        t.to_string()
    }
}

/// 校验并拼出监听地址串；`port == 0` 表示由系统分配（便于测试）。
pub fn listen_addr(host: &str, port: u16) -> AppResult<String> {
    let host = normalize_listen_host(host);
    if host.contains(':') && !host.starts_with('[') {
        // IPv6 字面量需要加方括号
        return Ok(format!("[{host}]:{port}"));
    }
    Ok(format!("{host}:{port}"))
}

/// 探测本地端口是否已被占用（供 UI / 预检使用）。
#[allow(dead_code)]
pub async fn is_port_free(addr: &str) -> bool {
    match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => {
            drop(l);
            true
        }
        Err(_) => false,
    }
}

/// 单条本地连接 ↔ SSH channel 的双向泵。
///
/// - 本地 → SSH：`read` → `channel.data(&buf[..])`；本地 EOF 时发 `EOF`
/// - SSH → 本地：`channel.wait()` → `Data`/`ExtendedData` → `write_all`；
///   `Eof` / `Close` / 通道耗尽 → 关闭本地写端并退出
///
/// `shutdown` 用于转发被停止时打断阻塞。
pub async fn bridge(
    mut local: TcpStream,
    mut channel: russh::Channel<russh::client::Msg>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> AppResult<()> {
    let originator = local
        .peer_addr()
        .map(|a: SocketAddr| a.to_string())
        .unwrap_or_default();
    tracing::debug!(originator, "forward connection established");

    let mut buf = vec![0u8; 32 * 1024];
    let mut local_eof = false;

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                // Sender 已被 drop（转发条目被移除）也视为"该停了"，
                // 否则 changed() 会持续返回 Err 造成忙等。
                match changed {
                    Ok(()) => {
                        if *shutdown.borrow() { break }
                    }
                    Err(_) => break,
                }
            }
            r = local.read(&mut buf), if !local_eof => {
                match r {
                    Ok(0) => {
                        local_eof = true;
                        let _ = channel.eof().await;
                    }
                    Ok(n) => {
                        if channel.data(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "forward local read error");
                        break;
                    }
                }
            }
            msg = channel.wait() => {
                match msg {
                    Some(russh::ChannelMsg::Data { data }) => {
                        if local.write_all(&data[..]).await.is_err() {
                            break;
                        }
                    }
                    Some(russh::ChannelMsg::ExtendedData { data, .. }) => {
                        // direct-tcpip 的 stderr 语义不适用；写入以保持字节序完整
                        if local.write_all(&data[..]).await.is_err() {
                            break;
                        }
                    }
                    Some(russh::ChannelMsg::Eof) => {
                        let _ = local.shutdown().await;
                        break;
                    }
                    Some(russh::ChannelMsg::Close) | None => break,
                    Some(_) => {}
                }
            }
        }
    }

    let _ = channel.eof().await;
    let _ = channel.close().await;
    let _ = local.shutdown().await;
    Ok(())
}

/// 便于上层把 `Bind` 失败统一映射为 `AppError::Forward`。
#[allow(dead_code)]
pub fn bind_error(addr: &str, e: impl std::fmt::Display) -> AppError {
    AppError::Forward(format!("listen failed: {addr}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_empty_host() {
        assert_eq!(normalize_listen_host(""), "127.0.0.1");
        assert_eq!(normalize_listen_host("  "), "127.0.0.1");
        assert_eq!(normalize_listen_host("127.0.0.1"), "127.0.0.1");
    }

    #[test]
    fn builds_listen_addr() {
        assert_eq!(listen_addr("127.0.0.1", 8080).unwrap(), "127.0.0.1:8080");
        assert_eq!(listen_addr("", 0).unwrap(), "127.0.0.1:0");
    }

    #[test]
    fn brackets_ipv6() {
        assert_eq!(listen_addr("::1", 22).unwrap(), "[::1]:22");
    }
}
