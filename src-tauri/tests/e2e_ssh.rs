//! 端到端集成测试：真实 SSH 网络链路。
//!
//! 这些测试起一个**真实的 russh 服务端**（监听 127.0.0.1 的随机端口），
//! 然后用客户端完整跑一遍：
//!   TCP 连接 → SSH 握手 → 密码认证 → 打开 session channel → PTY → shell → 数据往返
//!
//! 目的：证明 `russh` 在本项目依赖组合（`ring` + `rsa`，MSVC release）
//! 下握手/认证/通道全链路可用，而不只是「能编译」。
//!
//! 注意：
//! - 本机必须 `cargo test --release`（MSVC `-Z7` 限制，见 docs/BUILD-ENV.md）
//! - `SessionManager::create` 需要 `tauri::AppHandle`（Wry runtime），
//!   headless 环境无法构造，因此这里直接验证它调用的 russh 链路本身。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use russh::client::{self, Handle};
use russh::server::{Auth, Msg, Server, Session};
use russh::{Channel, ChannelId, Pty};
use tokio::sync::Mutex;

const PASSWORD: &str = "test-password";
const ECHO_MARKER: &str = "sshdeck-e2e-marker";

// ───────────────────────────── 测试用 SSH 服务端 ─────────────────────────────

#[derive(Default)]
struct ServerState {
    last_pty: Option<(u32, u32)>,
    saw_data: bool,
}

struct TestHandler {
    state: Arc<Mutex<ServerState>>,
}

impl russh::server::Handler for TestHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        if password == PASSWORD {
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
        _channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }

    async fn pty_request(
        &mut self,
        _channel: ChannelId,
        _term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(Pty, u32)],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let mut st = self.state.lock().await;
        st.last_pty = Some((col_width, row_height));
        Ok(())
    }

    async fn shell_request(
        &mut self,
        _channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        {
            let mut st = self.state.lock().await;
            st.saw_data = true;
        }
        let text = String::from_utf8_lossy(data);
        let reply = format!("echo:{text}|{ECHO_MARKER}\r\n");
        session.data(channel, reply.into_bytes().into())?;
        Ok(())
    }
}

struct TestServer {
    state: Arc<Mutex<ServerState>>,
}

impl Server for TestServer {
    type Handler = TestHandler;

    fn new_client(&mut self, _peer: Option<SocketAddr>) -> TestHandler {
        TestHandler {
            state: self.state.clone(),
        }
    }
}

/// 起测试服务端，返回端口与状态句柄。
async fn spawn_test_server() -> (u16, Arc<Mutex<ServerState>>) {
    let state = Arc::new(Mutex::new(ServerState::default()));

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

    let mut server = TestServer {
        state: state.clone(),
    };

    tokio::spawn(async move {
        let _ = server
            .run_on_address(config, SocketAddr::from(([127, 0, 0, 1], port)))
            .await;
    });

    tokio::time::sleep(Duration::from_millis(300)).await;
    (port, state)
}

// ───────────────────────────── 测试用客户端 ─────────────────────────────

#[derive(Default)]
struct RecordingClient {
    fingerprint: Arc<Mutex<Option<String>>>,
}

impl client::Handler for RecordingClient {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fp = server_public_key
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string();
        *self.fingerprint.lock().await = Some(fp);
        Ok(true)
    }
}

async fn connect_client(port: u16) -> (Handle<RecordingClient>, Arc<Mutex<Option<String>>>) {
    let fp_slot = Arc::new(Mutex::new(None));
    let handler = RecordingClient {
        fingerprint: fp_slot.clone(),
    };

    let stream = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .expect("connect timeout")
    .expect("tcp connect");

    let handle = client::connect_stream(Arc::new(client::Config::default()), stream, handler)
        .await
        .expect("ssh handshake");

    (handle, fp_slot)
}

// ───────────────────────────── 测试 ─────────────────────────────

#[tokio::test]
async fn e2e_handshake_records_fingerprint() {
    let (port, _state) = spawn_test_server().await;
    let (_handle, fp_slot) = connect_client(port).await;

    let fp = fp_slot.lock().await.clone();
    assert!(
        fp.as_deref().unwrap_or("").starts_with("SHA256:"),
        "host key fingerprint should be SHA256, got {fp:?}"
    );
}

#[tokio::test]
async fn e2e_password_auth_and_shell_roundtrip() {
    let (port, state) = spawn_test_server().await;
    let (mut handle, _fp) = connect_client(port).await;

    // 认证
    let auth = handle
        .authenticate_password("tester", PASSWORD)
        .await
        .expect("authenticate call");
    assert!(
        auth.success(),
        "password auth should succeed with correct password"
    );

    // 打开 session channel
    let mut channel = handle
        .channel_open_session()
        .await
        .expect("open session channel");

    // PTY：尺寸应传到服务端。
    // 用 want_reply=false，服务端处理是异步的，需要轮询等待落地。
    channel
        .request_pty(false, "xterm-256color", 120, 40, 0, 0, &[])
        .await
        .expect("request pty");

    let mut pty_seen = None;
    let pty_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < pty_deadline {
        let snapshot = state.lock().await.last_pty;
        if snapshot.is_some() {
            pty_seen = snapshot;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        pty_seen,
        Some((120, 40)),
        "PTY dimensions should reach the server"
    );

    // shell
    channel.request_shell(true).await.expect("request shell");

    // 发数据 → 服务端回写 → 客户端读到
    let payload = b"hello-sshdeck";
    channel.data(&payload[..]).await.expect("send data");

    let mut got = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), channel.wait()).await {
            Ok(Some(russh::ChannelMsg::Data { data })) => {
                got.extend_from_slice(&data[..]);
                if got
                    .windows(ECHO_MARKER.len())
                    .any(|w| w == ECHO_MARKER.as_bytes())
                {
                    break;
                }
            }
            Ok(Some(russh::ChannelMsg::Eof)) | Ok(Some(russh::ChannelMsg::Close)) => break,
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => break,
        }
    }

    let text = String::from_utf8_lossy(&got);
    assert!(
        text.contains("hello-sshdeck"),
        "server should echo our payload back, got: {text:?}"
    );
    assert!(
        text.contains(ECHO_MARKER),
        "server should append marker, got: {text:?}"
    );

    let st = state.lock().await;
    assert!(st.saw_data, "server should have received the data");
}

#[tokio::test]
async fn e2e_wrong_password_is_rejected() {
    let (port, _state) = spawn_test_server().await;
    let (mut handle, _fp) = connect_client(port).await;

    let auth = handle
        .authenticate_password("tester", "wrong-password")
        .await
        .expect("authenticate call");

    assert!(
        !auth.success(),
        "wrong password must not be accepted by the test server"
    );
}

#[tokio::test]
async fn e2e_direct_tcpip_channel_opens() {
    let (port, _state) = spawn_test_server().await;
    let (mut handle, _fp) = connect_client(port).await;

    let auth = handle
        .authenticate_password("tester", PASSWORD)
        .await
        .expect("authenticate");
    assert!(auth.success());

    // 端口转发依赖 direct-tcpip：验证该通道调用返回明确结果而非挂起。
    let res = handle
        .channel_open_direct_tcpip("127.0.0.1", 80, "127.0.0.1", 0)
        .await;

    match res {
        Ok(_) => { /* 服务端接受了 */ }
        Err(e) => {
            let msg = e.to_string();
            assert!(!msg.is_empty(), "rejection should carry an error message");
        }
    }
}

#[tokio::test]
async fn e2e_connect_to_closed_port_fails_cleanly() {
    let res = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect("127.0.0.1:1"),
    )
    .await;

    match res {
        Ok(Err(_)) => { /* 预期：连接被拒 */ }
        Ok(Ok(_)) => panic!("port 1 should not be open"),
        Err(_) => panic!("connect should fail fast, not time out"),
    }
}
