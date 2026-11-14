//! 端到端集成测试：known_hosts 严格校验（真实 russh 服务端）。
//!
//! 覆盖安全关键路径：
//! - strict + 空 known_hosts → 拒绝，且不落盘
//! - strict + 用户在指纹确认后 pin 的公钥 → 放行并写入 known_hosts
//! - accept_new → 首次自动记录；再次连接走 `verified`
//! - known_hosts 中记录的是另一把密钥 → 一律拒绝（MISMATCH）
//!
//! 注意：本机必须 `cargo test --release`（MSVC `-Z7` 限制，见 docs/BUILD-ENV.md）。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use modern_sshdeck_lib::ssh::known_hosts::{self, HostKeyPolicy};
use modern_sshdeck_lib::ssh::session::ClientHandler;
use russh::client;
use russh::server::{Server, Session};

// ───────────────────────────── 测试服务端 ─────────────────────────────

struct TestServer;

struct TestHandler;

impl russh::server::Handler for TestHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, _password: &str) -> Result<russh::server::Auth, Self::Error> {
        Ok(russh::server::Auth::Accept)
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
        TestHandler
    }
}

/// 起一个只做握手/认证的测试服务端，返回 (端口, 主机公钥)。
async fn spawn_test_server() -> (u16, russh::keys::PublicKey) {
    let key = russh::keys::PrivateKey::random(
        &mut russh::keys::signature::rand_core::OsRng,
        russh::keys::Algorithm::Ed25519,
    )
    .expect("generate host key");
    let public = key.public_key().clone();

    let config = std::sync::Arc::new(russh::server::Config {
        keys: vec![key],
        ..Default::default()
    });

    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
        probe.local_addr().expect("probe addr").port()
    };

    let mut server = TestServer;
    tokio::spawn(async move {
        let _ = server
            .run_on_address(config, SocketAddr::from(([127, 0, 0, 1], port)))
            .await;
    });

    tokio::time::sleep(Duration::from_millis(300)).await;
    (port, public)
}

// ───────────────────────────── 辅助 ─────────────────────────────

fn temp_known_hosts(tag: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "sshdeck-hk-{tag}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    (dir.clone(), dir.join("known_hosts"))
}

/// 用真实握手跑一次 `ClientHandler`，返回 (是否成功, 拒绝原因)。
async fn try_handshake(
    port: u16,
    path: &Path,
    policy: HostKeyPolicy,
    trusted: Option<russh::keys::PublicKey>,
) -> (bool, Option<String>) {
    let stream = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .expect("connect timeout")
    .expect("tcp connect");

    let (handler, slot) = ClientHandler::new(
        "127.0.0.1".to_string(),
        port,
        policy,
        path.to_path_buf(),
        trusted,
        None,
    );

    let result =
        client::connect_stream(std::sync::Arc::new(client::Config::default()), stream, handler)
            .await;

    let outcome = slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    (result.is_ok(), outcome.rejection)
}

// ───────────────────────────── 测试 ─────────────────────────────

#[tokio::test]
async fn strict_rejects_unknown_host_and_does_not_write() {
    let (port, _server_key) = spawn_test_server().await;
    let (dir, path) = temp_known_hosts("strict-unknown");

    let (ok, rejection) = try_handshake(port, &path, HostKeyPolicy::Strict, None).await;

    assert!(!ok, "strict 必须拒绝未知主机");
    let reason = rejection.expect("must carry a rejection reason");
    assert!(
        reason.contains("is not in "),
        "拒绝原因要能区分「未知」以便前端走指纹确认，got: {reason}"
    );
    assert!(!path.exists(), "被拒绝的连接绝不能写入 known_hosts");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn strict_accepts_pinned_key_and_records_it() {
    let (port, server_key) = spawn_test_server().await;
    let (dir, path) = temp_known_hosts("strict-pinned");

    let (ok, rejection) = try_handshake(
        port,
        &path,
        HostKeyPolicy::Strict,
        Some(server_key.clone()),
    )
    .await;
    assert!(ok, "用户确认过的公钥应放行，rejection={rejection:?}");

    let recorded = std::fs::read_to_string(&path).expect("known_hosts written");
    let expected = server_key.to_openssh().expect("encode server key");
    assert!(
        recorded.contains(&expected),
        "known_hosts 里应写入服务端公钥，got: {recorded}"
    );
    assert_eq!(recorded.lines().count(), 1, "只应写入一行: {recorded}");
    assert_eq!(
        known_hosts::verify_file(&path, "127.0.0.1", port, &server_key).expect("verify"),
        known_hosts::HostKeyVerdict::Match
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn accept_new_learns_then_verifies() {
    let (port, server_key) = spawn_test_server().await;
    let (dir, path) = temp_known_hosts("accept-new");

    let (ok, _) = try_handshake(port, &path, HostKeyPolicy::AcceptNew, None).await;
    assert!(ok, "accept_new 应放行未知主机并记录");
    assert_eq!(
        known_hosts::verify_file(&path, "127.0.0.1", port, &server_key).expect("verify"),
        known_hosts::HostKeyVerdict::Match
    );

    // 第二次连接：已知 → 无需再写
    let before = std::fs::read_to_string(&path).expect("read");
    let (ok, _) = try_handshake(port, &path, HostKeyPolicy::AcceptNew, None).await;
    assert!(ok);
    let after = std::fs::read_to_string(&path).expect("read");
    assert_eq!(before, after, "已验证的主机不应重复写入");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn changed_host_key_is_rejected_even_in_accept_new() {
    let (port, _server_key) = spawn_test_server().await;
    let (dir, path) = temp_known_hosts("mismatch");

    // 伪造一条「同类型但不同密钥」的记录，模拟服务端被替换 / MITM
    let impostor = russh::keys::PrivateKey::random(
        &mut russh::keys::signature::rand_core::OsRng,
        russh::keys::Algorithm::Ed25519,
    )
    .expect("generate impostor key");
    known_hosts::learn_file(&path, "127.0.0.1", port, impostor.public_key())
        .expect("seed known_hosts");

    let (ok, rejection) = try_handshake(port, &path, HostKeyPolicy::AcceptNew, None).await;
    assert!(!ok, "密钥变更必须拒绝连接");
    let reason = rejection.expect("must carry a rejection reason");
    assert!(
        reason.contains("HOST KEY MISMATCH"),
        "错误信息必须明确提示密钥变更，got: {reason}"
    );

    // 即便用户 pin 了当前服务端密钥，也不能绕过「变更」判定
    let (port2, server_key2) = spawn_test_server().await;
    let (dir2, path2) = temp_known_hosts("mismatch-pinned");
    known_hosts::learn_file(&path2, "127.0.0.1", port2, impostor.public_key())
        .expect("seed known_hosts");
    let (ok2, rejection2) =
        try_handshake(port2, &path2, HostKeyPolicy::Strict, Some(server_key2)).await;
    assert!(!ok2, "变更的密钥不能被 pin 绕过");
    assert!(rejection2
        .expect("reason")
        .contains("HOST KEY MISMATCH"));

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

#[tokio::test]
async fn insecure_still_connects_on_mismatch() {
    let (port, _server_key) = spawn_test_server().await;
    let (dir, path) = temp_known_hosts("insecure");

    let impostor = russh::keys::PrivateKey::random(
        &mut russh::keys::signature::rand_core::OsRng,
        russh::keys::Algorithm::Ed25519,
    )
    .expect("generate impostor key");
    known_hosts::learn_file(&path, "127.0.0.1", port, impostor.public_key())
        .expect("seed known_hosts");

    let (ok, _) = try_handshake(port, &path, HostKeyPolicy::Insecure, None).await;
    assert!(ok, "insecure 策略用于排障，应放行");

    let _ = std::fs::remove_dir_all(&dir);
}
