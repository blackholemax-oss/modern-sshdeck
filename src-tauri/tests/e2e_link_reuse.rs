//! 端到端集成测试：SSH 连接复用（多路会话共享一条底层连接）。
//!
//! 用真实的 russh 服务端统计「握手次数」（`Server::new_client` 调用次数）：
//! - 第二次用相同 host/port/user/凭据连接时，服务端**不应**再收到新连接；
//! - 连接池统计里 `handshakes` 不变、`reuses` +1、link 的 `sessions` 增加；
//! - 不同密码 → 属于不同身份 → 必须新建连接。
//!
//! 注意：本机必须 `cargo test --release`（MSVC `-Z7` 限制，见 docs/BUILD-ENV.md）。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use modern_sshdeck_lib::ipc::dto::{AuthSpec, ConnectRequest};
use modern_sshdeck_lib::ssh::manager::SessionManager;
use russh::server::{Auth, Server, Session};

// ───────────────────────────── 测试服务端 ─────────────────────────────

struct TestServer {
    connections: Arc<AtomicUsize>,
}

struct TestHandler;

impl russh::server::Handler for TestHandler {
    type Error = russh::Error;

    async fn auth_password(
        &mut self,
        _user: &str,
        password: &str,
    ) -> Result<Auth, Self::Error> {
        // 两种密码都接受：它们代表「同一主机不同凭据」的两个身份
        if password == "test-password" || password == "another-password" {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            })
        }
    }

    async fn channel_open_session(
        &mut self,
        _channel: russh::Channel<russh::server::Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

impl Server for TestServer {
    type Handler = TestHandler;

    fn new_client(&mut self, _peer: Option<SocketAddr>) -> TestHandler {
        self.connections.fetch_add(1, Ordering::SeqCst);
        TestHandler
    }
}

/// 起测试服务端，返回 (端口, 已接受的 TCP 连接数)。
async fn spawn_test_server() -> (u16, Arc<AtomicUsize>) {
    let key = russh::keys::PrivateKey::random(
        &mut russh::keys::signature::rand_core::OsRng,
        russh::keys::Algorithm::Ed25519,
    )
    .expect("generate host key");

    let config = Arc::new(russh::server::Config {
        keys: vec![key],
        ..Default::default()
    });

    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
        probe.local_addr().expect("probe addr").port()
    };

    let connections = Arc::new(AtomicUsize::new(0));
    let mut server = TestServer {
        connections: connections.clone(),
    };
    tokio::spawn(async move {
        let _ = server
            .run_on_address(config, SocketAddr::from(([127, 0, 0, 1], port)))
            .await;
    });

    tokio::time::sleep(Duration::from_millis(300)).await;
    (port, connections)
}

/// 每条测试用独立 known_hosts，避免污染真实的 ~/.ssh/known_hosts。
fn temp_known_hosts(tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "sshdeck-reuse-{tag}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir.join("known_hosts").to_string_lossy().to_string()
}

fn request(port: u16, password: &str) -> ConnectRequest {
    ConnectRequest {
        host_id: None,
        host: "127.0.0.1".into(),
        port,
        username: "tester".into(),
        auth: AuthSpec::Password {
            password: password.into(),
        },
        cols: 80,
        rows: 24,
        trust_host_key: None,
    }
}

// ───────────────────────────── 测试 ─────────────────────────────

#[tokio::test]
async fn second_session_reuses_the_same_transport() {
    let (port, connections) = spawn_test_server().await;
    let manager = SessionManager::new();

    // 未知主机在默认 strict 下会被拒绝：这里用临时 known_hosts + accept_new
    manager.apply_host_key_settings("accept_new", &temp_known_hosts("reuse"));

    let first = manager
        .connect_headless(request(port, "test-password"), None)
        .await
        .expect("first headless link");
    assert_eq!(connections.load(Ordering::SeqCst), 1, "应只建一条 TCP 连接");

    let second = manager
        .connect_headless(request(port, "test-password"), None)
        .await
        .expect("second headless link");

    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "第二次连接必须复用底层连接，不能再握手"
    );

    let stats = manager.pool_stats().await;
    assert_eq!(stats.handshakes, 1, "累计握手次数");
    assert_eq!(stats.reuses, 1, "累计复用次数");
    assert_eq!(stats.links.len(), 1, "池里只应有一条链路");
    assert_eq!(stats.links[0].sessions, 2, "两个会话共享同一条链路");
    assert_eq!(stats.links[0].reuses, 1);

    // 两个会话指向同一条 link
    assert_eq!(
        first.link.id, second.link.id,
        "会话应挂同一条 link"
    );
}

#[tokio::test]
async fn different_credentials_do_not_share_a_link() {
    let (port, connections) = spawn_test_server().await;
    let manager = SessionManager::new();
    manager.apply_host_key_settings("accept_new", &temp_known_hosts("creds"));

    manager
        .connect_headless(request(port, "test-password"), None)
        .await
        .expect("first link");
    manager
        .connect_headless(request(port, "another-password"), None)
        .await
        .expect("different credentials must create their own link");

    assert_eq!(
        connections.load(Ordering::SeqCst),
        2,
        "不同凭据是不同身份，必须各自握手"
    );
    let stats = manager.pool_stats().await;
    assert_eq!(stats.handshakes, 2);
    assert_eq!(stats.reuses, 0);
    assert_eq!(stats.links.len(), 2);
}

#[tokio::test]
async fn link_is_kept_for_reuse_after_session_close() {
    let (port, connections) = spawn_test_server().await;
    let manager = SessionManager::new();
    manager.apply_host_key_settings("accept_new", &temp_known_hosts("idle"));

    let first = manager
        .connect_headless(request(port, "test-password"), None)
        .await
        .expect("first link");
    manager
        .close(&first.session_id().await)
        .await
        .expect("close");

    // 空闲保留（默认 300s），连接不能被立刻断开
    let stats = manager.pool_stats().await;
    assert_eq!(stats.links.len(), 1, "空闲连接应保留供复用");
    assert_eq!(stats.links[0].sessions, 0, "引用已归还");

    manager
        .connect_headless(request(port, "test-password"), None)
        .await
        .expect("reuse idle link");
    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "空闲连接应被复用而不是重新握手"
    );
}

#[tokio::test]
async fn disabling_reuse_creates_a_new_link_each_time() {
    let (port, connections) = spawn_test_server().await;
    let manager = SessionManager::new();
    manager.apply_host_key_settings("accept_new", &temp_known_hosts("noreuse"));
    manager.apply_reuse_settings(false, 0);

    let first = manager
        .connect_headless(request(port, "test-password"), None)
        .await
        .expect("first link");
    manager
        .connect_headless(request(port, "test-password"), None)
        .await
        .expect("second link");

    assert_eq!(
        connections.load(Ordering::SeqCst),
        2,
        "关闭复用后每次都要新建连接"
    );

    // 关闭复用时，会话关闭即断开链路
    manager
        .close(&first.session_id().await)
        .await
        .expect("close");
    let stats = manager.pool_stats().await;
    assert!(
        stats.links.len() < 2,
        "关闭复用后不应保留已无引用的链路，got {}",
        stats.links.len()
    );
}
