//! SSH 客户端配置。

use std::time::Duration;

/// 连接与保活参数。
#[derive(Debug, Clone)]
pub struct SshOptions {
    /// 建连超时（TCP connect）
    pub connect_timeout: Duration,
    /// 保活间隔；None 关闭
    pub keepalive: Option<Duration>,
    /// 认证后打开 shell 的超时
    pub shell_timeout: Duration,
}

impl Default for SshOptions {
    fn default() -> Self {
        Self {
            // 内网/公网常见值：握手 + 认证通常 < 10s，20s 给慢链路留余量
            connect_timeout: Duration::from_secs(20),
            keepalive: Some(Duration::from_secs(30)),
            shell_timeout: Duration::from_secs(15),
        }
    }
}

impl SshOptions {
    #[allow(dead_code)]
    pub fn with_connect_timeout(mut self, d: Duration) -> Self {
        self.connect_timeout = d;
        self
    }

    #[allow(dead_code)]
    pub fn with_keepalive(mut self, d: Option<Duration>) -> Self {
        self.keepalive = d;
        self
    }

    /// 终端类型。`xterm-256color` 是绝大多数 TUI 程序（vim/htop/tmux）的首选。
    pub const TERM: &'static str = "xterm-256color";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_are_sane() {
        let o = SshOptions::default();
        assert!(o.connect_timeout >= Duration::from_secs(10));
        assert!(o.keepalive.is_some());
        assert_eq!(SshOptions::TERM, "xterm-256color");
    }
}
