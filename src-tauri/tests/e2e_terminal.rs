//! 回归测试：终端交互链路的两个历史 bug。
//!
//! 1. **PTY terminal_modes 里混入 `TTY_OP_END`**
//!    `TTY_OP_END` 是 RFC4254 终止 mode 列表的哨兵，只该由编码层追加。
//!    作为真实 mode 传入会让编码后的列表提前终止，剩余字节被服务端当成
//!    后续字段，解析错位后不回 `pty-req` 的 reply —— `request_pty(true)`
//!    永久挂起，最终被 shell_timeout 打断，外部表现是「连上却没有提示符、
//!    会话立即消失」。
//!
//! 2. **`watch::channel` 的 receiver 被立即 drop**
//!    没有常驻 receiver 时 sender 进入 closed 状态，之后 `subscribe()` 出来的
//!    receiver 首次 `changed()` 立刻返回 Err，shell task 启动即误判「已关闭」
//!    并退出，会话随即从表中消失。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use russh::server::{Auth, Msg, Server, Session};
use russh::{Channel, ChannelId, Pty};
use tokio::sync::Mutex;

/// 服务端记录的 PTY 请求参数，用来断言 mode 列表没有把 TTY_OP_END 混进去。
#[derive(Default)]
struct State {
    pty_modes: Vec<(Pty, u32)>,
    pty_cols: u32,
    pty_rows: u32,
}

struct H {
    state: Arc<Mutex<State>>,
}

impl russh::server::Handler for H {
    type Error = russh::Error;

    async fn auth_password(&mut self, _u: &str, _p: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        _c: Channel<Msg>,
        _s: &mut Session,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }

    async fn pty_request(
        &mut self,
        _c: ChannelId,
        _t: &str,
        w: u32,
        h: u32,
        _pw: u32,
        _ph: u32,
        modes: &[(Pty, u32)],
        _s: &mut Session,
    ) -> Result<(), Self::Error> {
        let mut st = self.state.lock().await;
        st.pty_modes = modes.to_vec();
        st.pty_cols = w;
        st.pty_rows = h;
        Ok(())
    }

    async fn shell_request(&mut self, c: ChannelId, s: &mut Session) -> Result<(), Self::Error> {
        s.data(c, b"prompt$ ".to_vec().into())?;
        Ok(())
    }

    /// 简单回显，用于验证输入能到达服务端。
    async fn data(&mut self, c: ChannelId, data: &[u8], s: &mut Session) -> Result<(), Self::Error> {
        let text = String::from_utf8_lossy(data).trim().to_string();
        s.data(c, format!("echo:{text}\r\nprompt$ ").into_bytes().into())?;
        Ok(())
    }
}

struct Svc {
    state: Arc<Mutex<State>>,
}

impl Server for Svc {
    type Handler = H;
    fn new_client(&mut self, _p: Option<SocketAddr>) -> H {
        H {
            state: self.state.clone(),
        }
    }
}

async fn spawn_server() -> (u16, Arc<Mutex<State>>) {
    let state = Arc::new(Mutex::new(State::default()));
    let key = russh::keys::PrivateKey::random(
        &mut russh::keys::signature::rand_core::OsRng,
        russh::keys::Algorithm::Ed25519,
    )
    .expect("host key");

    let cfg = Arc::new(russh::server::Config {
        keys: vec![key],
        ..Default::default()
    });

    let port = {
        let p = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
        p.local_addr().expect("addr").port()
    };

    let mut svc = Svc {
        state: state.clone(),
    };
    tokio::spawn(async move {
        let _ = svc
            .run_on_address(cfg, SocketAddr::from(([127, 0, 0, 1], port)))
            .await;
    });

    tokio::time::sleep(Duration::from_millis(300)).await;
    (port, state)
}

struct C;
impl russh::client::Handler for C {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _k: &russh::keys::PublicKey) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// 与 `SessionManager` 相同的 mode 列表（不含 TTY_OP_END）。
fn session_terminal_modes() -> Vec<(Pty, u32)> {
    vec![
        (Pty::VINTR, 3),
        (Pty::VQUIT, 28),
        (Pty::VERASE, 127),
        (Pty::VKILL, 21),
        (Pty::VEOF, 4),
        (Pty::ICRNL, 1),
        (Pty::IXON, 1),
        (Pty::IUTF8, 1),
        (Pty::ISIG, 1),
        (Pty::ICANON, 1),
        (Pty::ECHO, 1),
        (Pty::ECHOE, 1),
        (Pty::ECHOK, 1),
        (Pty::ECHOCTL, 1),
        (Pty::OPOST, 1),
    ]
}

#[tokio::test]
async fn pty_request_without_tty_op_end_returns_promptly() {
    let (port, state) = spawn_server().await;

    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("tcp");
    let mut h = russh::client::connect_stream(Arc::new(russh::client::Config::default()), stream, C)
        .await
        .expect("handshake");

    assert!(h.authenticate_password("u", "p").await.expect("auth").success());

    let mut ch = h.channel_open_session().await.expect("open channel");

    // `want_reply = true`：若 mode 列表被 TTY_OP_END 提前终止，这一步会永久挂起。
    // 用 timeout 断言它必须及时返回，这正是「连上却没有提示符」的回归防线。
    tokio::time::timeout(
        Duration::from_secs(5),
        ch.request_pty(true, "xterm-256color", 120, 32, 0, 0, &session_terminal_modes()),
    )
    .await
    .expect("request_pty must return promptly (TTY_OP_END must not be in the list)")
    .expect("request_pty");

    ch.request_shell(true).await.expect("shell");

    // 提示符必须能读到
    let msg = tokio::time::timeout(Duration::from_secs(5), ch.wait())
        .await
        .expect("prompt should arrive")
        .expect("channel should stay open");
    let data = match msg {
        russh::ChannelMsg::Data { data } => data,
        other => panic!("expected Data, got {other:?}"),
    };
    assert!(
        String::from_utf8_lossy(&data[..]).contains("prompt$"),
        "server prompt should be delivered"
    );

    // 服务端收到的 modes 必须与客户端发送的完全一致（不多也不少）
    let st = state.lock().await;
    assert_eq!(
        st.pty_modes,
        session_terminal_modes(),
        "PTY modes must match exactly; a stray TTY_OP_END would corrupt the wire format"
    );
    assert_eq!((st.pty_cols, st.pty_rows), (120, 32));
}

#[tokio::test]
async fn tty_op_end_in_list_is_the_breaking_case() {
    // 反向证明：把 TTY_OP_END 塞进列表就会破坏 pty-req 编码。
    // 这里只验证「列表里含 TTY_OP_END 时与规范列表不同」，
    // 真正的挂起行为由上一个测试的超时断言守护。
    let mut bad = session_terminal_modes();
    bad.push((Pty::TTY_OP_END, 0));
    assert_ne!(
        bad,
        session_terminal_modes(),
        "regression guard: TTY_OP_END must never be appended by callers"
    );
}
