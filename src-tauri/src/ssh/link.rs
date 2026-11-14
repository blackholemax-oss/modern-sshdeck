//! SSH 连接复用：一条 TCP + 一次握手的底层连接，被多个逻辑会话共享。
//!
//! ## 为什么需要
//!
//! 终端分屏、Agent 的 exec 通道、端口转发、SFTP 都只依赖「通道」，不依赖独立
//! 的 TCP 连接。此前每开一个 pane 都要重新 TCP 握手 + 认证，跨公网时单次
//! 300~800ms，分屏四块就是秒级等待；服务端也会按 MaxStartups/MaxSessions
//! 限制并发连接数。
//!
//! ## 做法
//!
//! - 连接的**身份**由 `link_key` 决定：host + port + username + 认证凭据指纹。
//!   同一身份的所有逻辑会话共享一个 [`SharedLink`]。
//! - 每个逻辑会话仍各自 `channel_open_session`（独立 PTY / 独立 shell），因此
//!   tab 隔离、退出码、窗口尺寸互不影响 —— 复用的只是传输层。
//! - 引用计数归零后连接**不立刻断开**：保留 `idle_ttl`（默认 300s）供下次
//!   复用；到期由后台回收任务优雅 disconnect。
//! - 凭据指纹用 HMAC-SHA1（进程内随机化密钥）计算，**原始密码不会进入 key**，
//!   也不会写日志。
//!
//! ## 失效语义
//!
//! 底层连接断了，所有共享它的会话会同时失效：`channel.wait()` 返回 `None`
//! → 各自的 shell task 走关闭流程；`SharedLink::closed` 广播给路由器与回收器。
//! 端口转发任务据此交给 `ForwardSupervisor` 重连（见 `forward/supervisor.rs`）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use hmac::{Hmac, Mac};
use sha1::Sha1;
use tokio::sync::{watch, Mutex};

use crate::ipc::dto::AuthSpec;
use crate::ssh::session::ClientHandler;

type HmacSha1 = Hmac<Sha1>;

/// 连接身份键：host/port/username 明文 + 凭据指纹。
///
/// 同一台机器、同一用户、同一凭据 → 同一个键 → 复用同一条 SSH 传输。
pub fn link_key(host: &str, port: u16, username: &str, auth: &AuthSpec) -> String {
    let (kind, secret) = match auth {
        AuthSpec::Password { password } => ("password", password.clone()),
        AuthSpec::Key {
            key_path,
            passphrase,
        } => (
            "key",
            format!(
                "{key_path}\u{0}{}",
                passphrase.as_deref().unwrap_or("").to_string()
            ),
        ),
    };

    // 固定盐：同一凭据在进程重启后仍得到同一指纹（不影响安全性：
    // 指纹只用于进程内匹配，且从不回传前端 / 写日志）。
    let mut mac = HmacSha1::new_from_slice(b"sshdeck/link-key/v1").expect("hmac accepts any key");
    mac.update(kind.as_bytes());
    mac.update(b"\0");
    mac.update(username.as_bytes());
    mac.update(b"\0");
    mac.update(secret.as_bytes());
    let digest = mac.finalize().into_bytes();
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();

    format!(
        "{kind}:{}:{port}:{}:{hex}",
        host.trim().to_ascii_lowercase(),
        username.trim()
    )
}

/// 一条被共享的底层 SSH 连接（TCP + 握手 + 认证已完成）。
pub struct SharedLink {
    pub id: String,
    pub key: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// 所有共享者共用的 russh 句柄。开 channel 时短暂持锁，channel 独立使用。
    pub handle: Mutex<russh::client::Handle<ClientHandler>>,
    /// 连接结束标志（`watch`）。断线 / 回收时置 true。
    pub closed: watch::Sender<bool>,
    /// 常驻 receiver：避免 sender 因「无接收者」被判定 closed。
    pub closed_guard: Mutex<watch::Receiver<bool>>,
    /// `-R` 远程转发的来连通道路由表。
    ///
    /// **放在链路上而不是会话上**：同一条连接只会有一种来连通道，
    /// 规则可能由任意共享者注册，路由表必须是连接级的。
    pub remote_routes: crate::ssh::session::RemoteRouter,
    /// 活跃共享者数量（含隐藏的后台转发会话）。
    pub refs: AtomicUsize,
    /// 这条连接被「复用」了多少次（新握手算一次 handshake，其余都是复用）。
    pub reuses: AtomicUsize,
    pub created_at: i64,
    /// 最近一次被使用 / 释放的时间（毫秒），用于空闲回收。
    pub last_used_ms: AtomicI64,
    /// 建立这条连接时的 known_hosts 校验结果（复用者共享同一次校验）。
    pub host_key: crate::ssh::session::HostKeyOutcome,
}

impl SharedLink {
    pub fn new(
        id: String,
        key: String,
        host: String,
        port: u16,
        username: String,
        handle: russh::client::Handle<ClientHandler>,
        host_key: crate::ssh::session::HostKeyOutcome,
        now_ms: i64,
    ) -> Self {
        let (closed, closed_rx) = watch::channel(false);
        Self {
            id,
            key,
            host,
            port,
            username,
            handle: Mutex::new(handle),
            closed,
            closed_guard: Mutex::new(closed_rx),
            remote_routes: crate::ssh::session::RemoteRouter::new(),
            refs: AtomicUsize::new(0),
            reuses: AtomicUsize::new(0),
            created_at: now_ms,
            last_used_ms: AtomicI64::new(now_ms),
            host_key,
        }
    }

    pub fn mark_closed(&self) {
        let _ = self.closed.send_replace(true);
    }

    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    /// 建立一个共享者的引用。
    pub fn acquire(&self) {
        self.refs.fetch_add(1, Ordering::SeqCst);
        self.touch();
    }

    /// 释放共享者引用；归零后交给空闲回收（而不是立即断开）。
    pub fn release(&self) {
        self.refs.fetch_sub(1, Ordering::SeqCst);
        self.touch();
    }

    pub fn touch(&self) {
        self.last_used_ms
            .store(crate::persist::store::now_millis(), Ordering::Relaxed);
    }

    pub fn idle_for(&self) -> Duration {
        let idle_ms = crate::persist::store::now_millis() - self.last_used_ms.load(Ordering::Relaxed);
        Duration::from_millis(idle_ms.max(0) as u64)
    }

    pub fn handle_is_closed(&self) -> bool {
        match self.handle.try_lock() {
            Ok(handle) => handle.is_closed(),
            Err(_) => false,
        }
    }
}

/// 连接池统计（`ssh_pool_stats` 的返回）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkStat {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// 当前共享者数量
    pub sessions: usize,
    /// 这条连接被复用的次数
    pub reuses: usize,
    pub created_at: i64,
    pub idle_ms: i64,
}

/// 连接池。
pub struct LinkPool {
    links: Mutex<HashMap<String, Arc<SharedLink>>>,
    /// 累计新建连接数（= 握手次数）
    handshakes: AtomicUsize,
    /// 累计复用次数
    reuses: AtomicUsize,
    /// 空闲回收阈值（同步锁：配置更新是同步入口）
    idle_ttl: std::sync::RwLock<Duration>,
}

impl Default for LinkPool {
    fn default() -> Self {
        Self::new()
    }
}

const DEFAULT_IDLE_TTL: Duration = Duration::from_secs(300);
/// 空闲连接回收的最长等待：超过这个时间的空闲连接一律断开。
const MAX_IDLE_TTL: Duration = Duration::from_secs(24 * 3600);

impl LinkPool {
    pub fn new() -> Self {
        Self {
            links: Mutex::new(HashMap::new()),
            handshakes: AtomicUsize::new(0),
            reuses: AtomicUsize::new(0),
            idle_ttl: std::sync::RwLock::new(DEFAULT_IDLE_TTL),
        }
    }

    pub fn set_idle_ttl(&self, ttl: Duration) {
        let ttl = ttl.min(MAX_IDLE_TTL);
        match self.idle_ttl.write() {
            Ok(mut guard) => *guard = ttl,
            Err(poisoned) => *poisoned.into_inner() = ttl,
        }
    }

    pub fn idle_ttl(&self) -> Duration {
        match self.idle_ttl.read() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    /// 取一条可用连接；已断开的一律剔除，避免复用死连接。
    pub async fn find(&self, key: &str) -> Option<Arc<SharedLink>> {
        let found = self.links.lock().await.get(key).cloned()?;
        if found.is_closed() || found.handle_is_closed() {
            self.links.lock().await.remove(key);
            return None;
        }
        found.touch();
        found.reuses.fetch_add(1, Ordering::Relaxed);
        self.reuses.fetch_add(1, Ordering::Relaxed);
        Some(found)
    }

    pub async fn insert(&self, link: Arc<SharedLink>) {
        self.handshakes.fetch_add(1, Ordering::Relaxed);
        self.links.lock().await.insert(link.key.clone(), link);
    }

    pub async fn remove(&self, key: &str) -> Option<Arc<SharedLink>> {
        self.links.lock().await.remove(key)
    }

    pub async fn get(&self, key: &str) -> Option<Arc<SharedLink>> {
        self.links.lock().await.get(key).cloned()
    }

    /// 断开一条连接（不等待服务端回包超过 2s）。
    pub async fn close_link(link: &Arc<SharedLink>, reason: &str) {
        link.mark_closed();
        let Ok(handle) = tokio::time::timeout(Duration::from_secs(2), link.handle.lock()).await else {
            tracing::debug!(link = %link.id, "skip graceful disconnect: handle busy");
            return;
        };
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            handle.disconnect(russh::Disconnect::ByApplication, reason, "en"),
        )
        .await;
    }

    /// 关闭所有空闲连接（关闭复用开关时立刻生效）。
    pub async fn close_idle_now(&self) -> usize {
        let candidates: Vec<Arc<SharedLink>> = {
            let links = self.links.lock().await;
            links
                .values()
                .filter(|l| l.refs.load(Ordering::SeqCst) == 0)
                .cloned()
                .collect()
        };
        let mut closed = 0;
        for link in candidates {
            self.links.lock().await.remove(&link.key);
            Self::close_link(&link, "reuse disabled").await;
            closed += 1;
        }
        closed
    }

    /// 回收空闲连接：引用为 0 且空闲超过 TTL。
    pub async fn reap_idle(&self) -> Vec<String> {
        let ttl = self.idle_ttl();
        let candidates: Vec<Arc<SharedLink>> = {
            let links = self.links.lock().await;
            links
                .values()
                .filter(|l| {
                    l.refs.load(Ordering::SeqCst) == 0
                        && !l.is_closed()
                        && l.idle_for() >= ttl
                })
                .cloned()
                .collect()
        };

        let mut reaped = Vec::new();
        for link in candidates {
            self.links.lock().await.remove(&link.key);
            Self::close_link(&link, "idle timeout").await;
            reaped.push(link.id.clone());
        }
        reaped
    }

    pub async fn stats(&self) -> (Vec<LinkStat>, usize, usize) {
        let links = self.links.lock().await;
        let mut out: Vec<LinkStat> = links
            .values()
            .filter(|l| !l.is_closed())
            .map(|l| LinkStat {
                id: l.id.clone(),
                host: l.host.clone(),
                port: l.port,
                username: l.username.clone(),
                sessions: l.refs.load(Ordering::SeqCst),
                reuses: l.reuses.load(Ordering::Relaxed),
                created_at: l.created_at,
                idle_ms: l.idle_for().as_millis() as i64,
            })
            .collect();
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        (
            out,
            self.handshakes.load(Ordering::Relaxed),
            self.reuses.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pw(p: &str) -> AuthSpec {
        AuthSpec::Password {
            password: p.to_string(),
        }
    }

    #[test]
    fn same_identity_shares_key() {
        let a = link_key("Host.Example", 22, "bob", &pw("secret"));
        let b = link_key("host.example ", 22, "bob", &pw("secret"));
        assert_eq!(a, b, "主机名大小写 / 空白不应影响复用");
    }

    #[test]
    fn different_secret_or_user_gets_different_key() {
        let base = link_key("h", 22, "bob", &pw("secret"));
        assert_ne!(base, link_key("h", 22, "bob", &pw("secret2")));
        assert_ne!(base, link_key("h", 22, "alice", &pw("secret")));
        assert_ne!(base, link_key("h", 2200, "bob", &pw("secret")));
        assert_ne!(
            base,
            link_key(
                "h",
                22,
                "bob",
                &AuthSpec::Key {
                    key_path: "~/.ssh/id_ed25519".into(),
                    passphrase: None
                }
            )
        );
    }

    #[test]
    fn key_never_contains_plaintext_secret() {
        let key = link_key("h", 22, "bob", &pw("super-secret-password"));
        assert!(!key.contains("super-secret-password"));
        // 指纹固定长度，避免泄漏长度信息
        let fp = key.rsplit(':').next().unwrap();
        assert_eq!(fp.len(), 16, "8 字节十六进制指纹");
    }

    #[test]
    fn passphrase_changes_key() {
        let with = link_key(
            "h",
            22,
            "bob",
            &AuthSpec::Key {
                key_path: "~/.ssh/id_ed25519".into(),
                passphrase: Some("p".into()),
            },
        );
        let without = link_key(
            "h",
            22,
            "bob",
            &AuthSpec::Key {
                key_path: "~/.ssh/id_ed25519".into(),
                passphrase: None,
            },
        );
        assert_ne!(with, without);
    }

    #[tokio::test]
    async fn pool_reuse_counter_and_idle_settings() {
        let pool = LinkPool::new();
        assert!(pool.find("nope").await.is_none());
        let (stats, handshakes, reuses) = pool.stats().await;
        assert!(stats.is_empty() && handshakes == 0 && reuses == 0);

        pool.set_idle_ttl(Duration::from_secs(60));
        assert_eq!(pool.idle_ttl(), Duration::from_secs(60));
        // 上限保护
        pool.set_idle_ttl(Duration::from_secs(10 * 24 * 3600));
        assert_eq!(pool.idle_ttl(), MAX_IDLE_TTL);
    }
}
