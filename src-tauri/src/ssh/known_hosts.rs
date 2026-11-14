//! `known_hosts` 严格校验。
//!
//! 这是防中间人攻击（MITM）的核心：每次握手都用服务端出示的主机公钥去比对
//! `known_hosts`，只有「已知且完全一致」才放行；「记录过但变了」一律视为攻击
//! 并拒绝。
//!
//! 支持 OpenSSH 的以下格式（与 `sshd` 手册一致）：
//!
//! - 明文主机名：`host`（默认端口 22）= 一行可写多个模式
//! - 非默认端口：`[host]:2222`
//! - 通配：`*.example.com`、`192.168.1.?`
//! - 取反：`!bad.example.com`
//! - 哈希主机名：`|1|base64(salt)|base64(HMAC-SHA1(salt, host))`
//! - 标记：`@revoked`（强制拒绝）、`@cert-authority`（本实现不支持证书，跳过）
//!
//! 解析刻意不复用 `russh` 内置的 `known_host_keys_path`：那个实现只做
//! 「主机名完全相等」的匹配，不认通配与取反，也不返回行号（错误信息无法定位）。

use std::io::Write;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha1::Sha1;

use crate::error::{AppError, AppResult};
use russh::keys::{HashAlg, PublicKey};

/// 主机密钥校验策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HostKeyPolicy {
    /// 只接受 `known_hosts` 中已记录且一致的密钥（默认）。
    ///
    /// 首次连接的未知主机会被拒绝，用户在前端确认指纹后，带上
    /// `trustHostKey` 重新连接即可完成「显式信任 + 落盘」。
    #[default]
    Strict,
    /// TOFU（accept-new）：未知主机的密钥自动记录；记录过但变了仍然拒绝。
    AcceptNew,
    /// 不校验（仅调试用）。仍然会记录指纹并写 WARN 日志。
    Insecure,
}

impl HostKeyPolicy {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "strict" => Some(Self::Strict),
            "accept_new" | "accept-new" | "tofu" => Some(Self::AcceptNew),
            "insecure" | "off" | "none" => Some(Self::Insecure),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::AcceptNew => "accept_new",
            Self::Insecure => "insecure",
        }
    }
}

/// 单次校验结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyVerdict {
    /// `known_hosts` 里有同类型的同一把密钥。
    Match,
    /// 没有任何与 (host, port) 匹配的记录。
    Unknown,
    /// 记录过该主机，但同类型密钥不同 —— 典型的 MITM / 服务端重装特征。
    Mismatch { line: usize },
    /// 命中 `@revoked`。
    Revoked { line: usize },
}

impl HostKeyVerdict {
    /// 是否可以直接放行（无需用户确认、无需落盘）。
    pub fn is_trusted(&self) -> bool {
        matches!(self, Self::Match)
    }

    /// 是否属于「已经记录过、但密钥变了」这类绝对不可放行的情况。
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::Mismatch { .. } | Self::Revoked { .. })
    }

    /// 前端展示 / 探针返回用的短标签。
    pub fn status(&self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Unknown => "unknown",
            Self::Mismatch { .. } => "mismatch",
            Self::Revoked { .. } => "revoked",
        }
    }

    /// 给用户看的中文/英文混合说明（错误信息统一为英文，便于日志检索）。
    pub fn describe(&self) -> String {
        match self {
            Self::Match => "host key matches known_hosts".into(),
            Self::Unknown => "host key is not present in known_hosts".into(),
            Self::Mismatch { line } => format!(
                "host key does not match the key recorded at known_hosts line {line}"
            ),
            Self::Revoked { line } => {
                format!("host key is marked @revoked at known_hosts line {line}")
            }
        }
    }
}

/// `known_hosts` 文件位置。
///
/// 优先级：`SSHDECK_KNOWN_HOSTS` 环境变量（便于测试 / 自定义）→ `~/.ssh/known_hosts`。
pub fn default_path() -> PathBuf {
    if let Ok(raw) = std::env::var("SSHDECK_KNOWN_HOSTS") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    match dirs::home_dir() {
        Some(home) => home.join(".ssh").join("known_hosts"),
        None => PathBuf::from(".ssh").join("known_hosts"),
    }
}

/// 用 `path` 指向的已知主机文件校验 `key`。
///
/// 文件不存在等价于「没有任何记录」（`Unknown`），而不是错误。
pub fn verify_file(path: &Path, host: &str, port: u16, key: &PublicKey) -> AppResult<HostKeyVerdict> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HostKeyVerdict::Unknown),
        Err(e) => {
            return Err(AppError::HostKey(format!(
                "read known_hosts {} failed: {e}",
                path.display()
            )))
        }
    };
    Ok(verify_str(&raw, host, port, key))
}

/// 纯字符串版本的校验（便于单测，不做 IO）。
pub fn verify_str(raw: &str, host: &str, port: u16, key: &PublicKey) -> HostKeyVerdict {
    let target = host_target(host, port);
    let mut matched = false;
    let mut mismatch: Option<usize> = None;

    for entry in parse_entries(raw) {
        if !patterns_match(&entry.patterns, &target) {
            continue;
        }
        // 证书不能被当普通公钥匹配，跳过（本实现不支持证书主机密钥）。
        if entry.cert_authority {
            continue;
        }
        let same_key = entry.key.key_data() == key.key_data();
        if entry.revoked {
            if same_key {
                return HostKeyVerdict::Revoked { line: entry.line };
            }
            continue;
        }
        if same_key {
            matched = true;
        } else if entry.key.algorithm() == key.algorithm() && mismatch.is_none() {
            mismatch = Some(entry.line);
        }
    }

    if matched {
        HostKeyVerdict::Match
    } else if let Some(line) = mismatch {
        HostKeyVerdict::Mismatch { line }
    } else {
        HostKeyVerdict::Unknown
    }
}

/// 把 `key` 追加到 `path`（不存在则创建，含父目录）。
///
/// 幂等：已经记录过同一把密钥时直接返回。
pub fn learn_file(path: &Path, host: &str, port: u16, key: &PublicKey) -> AppResult<()> {
    if let Ok(HostKeyVerdict::Match) = verify_file(path, host, port, key) {
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AppError::HostKey(format!("create {} failed: {e}", parent.display()))
            })?;
            restrict_dir_permissions(parent);
        }
    }

    let openssh = key
        .to_openssh()
        .map_err(|e| AppError::HostKey(format!("encode host key failed: {e}")))?;
    let line = format!("{} {}\n", host_target(host, port), openssh);

    let need_newline = match std::fs::read(path) {
        Ok(bytes) => bytes.last().is_some_and(|b| *b != b'\n'),
        Err(_) => false,
    };

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| AppError::HostKey(format!("open known_hosts {} failed: {e}", path.display())))?;
    if need_newline {
        file.write_all(b"\n")
            .map_err(|e| AppError::HostKey(format!("write known_hosts failed: {e}")))?;
    }
    file.write_all(line.as_bytes())
        .map_err(|e| AppError::HostKey(format!("write known_hosts failed: {e}")))?;
    file.flush()
        .map_err(|e| AppError::HostKey(format!("flush known_hosts failed: {e}")))?;
    restrict_file_permissions(path);
    Ok(())
}

/// 返回 `known_hosts` 中给 (host, port) 用的主机串。
///
/// 非 22 端口必须写成 `[host]:port`，这是 OpenSSH 的约定。
pub fn host_target(host: &str, port: u16) -> String {
    let host = host.trim().to_ascii_lowercase();
    if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    }
}

// ───────────────────────────── 解析 ─────────────────────────────

struct Entry {
    line: usize,
    revoked: bool,
    cert_authority: bool,
    patterns: Vec<String>,
    key: PublicKey,
}

/// 逐行解析；无法识别的行（注释、未知标记、损坏的 base64）直接跳过，
/// 但**不会**因为某一行坏了就放弃整个文件 —— 与 OpenSSH 的宽容度一致。
fn parse_entries(raw: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    for (idx, raw_line) in raw.lines().enumerate() {
        let line = raw_line.trim_matches(['\r', '\n']).trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut rest = line;
        let mut revoked = false;
        let mut cert_authority = false;
        if rest.starts_with('@') {
            let Some((marker, tail)) = rest.split_once(char::is_whitespace) else {
                continue;
            };
            match marker {
                "@revoked" => revoked = true,
                "@cert-authority" => cert_authority = true,
                // 未知标记：整行忽略，避免误判
                _ => continue,
            }
            rest = tail.trim_start();
        }
        let Some((hosts, key_part)) = rest.split_once(char::is_whitespace) else {
            continue;
        };
        let patterns: Vec<String> = hosts
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
        if patterns.is_empty() {
            continue;
        }
        let Ok(key) = PublicKey::from_openssh(key_part.trim()) else {
            continue;
        };
        out.push(Entry {
            line: idx + 1,
            revoked,
            cert_authority,
            patterns,
            key,
        });
    }
    out
}

/// 一组模式（逗号分隔）是否匹配目标：任一正向模式命中、且没有负向模式命中。
fn patterns_match(patterns: &[String], target: &str) -> bool {
    let mut hit = false;
    for pattern in patterns {
        if let Some(neg) = pattern.strip_prefix('!') {
            if single_pattern_matches(neg, target) {
                return false;
            }
        } else if single_pattern_matches(pattern, target) {
            hit = true;
        }
    }
    hit
}

fn single_pattern_matches(pattern: &str, target: &str) -> bool {
    if let Some(hashed) = pattern.strip_prefix("|1|") {
        return hashed_host_matches(hashed, target);
    }
    glob_match(&pattern.to_ascii_lowercase(), target)
}

/// `|1|base64(salt)|base64(HMAC-SHA1(salt, host))` 校验。
fn hashed_host_matches(hashed: &str, target: &str) -> bool {
    let mut parts = hashed.split('|');
    let (Some(salt_b64), Some(hash_b64)) = (parts.next(), parts.next()) else {
        return false;
    };
    let engine = base64::engine::general_purpose::STANDARD;
    let (Ok(salt), Ok(expected)) = (engine.decode(salt_b64), engine.decode(hash_b64)) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(&salt) else {
        return false;
    };
    mac.update(target.as_bytes());
    mac.verify_slice(&expected).is_ok()
}

/// 简易 glob：只支持 `*` 与 `?`（OpenSSH 的 known_hosts 也只有这两个）。
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    // 最近一个 `*` 的位置，以及它当前吞到的文本位置（回溯用）
    let mut star: Option<usize> = None;
    let mut mark = 0usize;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = ti;
        } else if let Some(star_pi) = star {
            pi = star_pi + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644));
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &Path) {}

#[cfg(unix)]
fn restrict_dir_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict_dir_permissions(_path: &Path) {}

/// 供 UI 展示的 SHA256 指纹。
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED25519_A: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
    const ED25519_B: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G2sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";
    /// 与 OpenSSH `HashKnownHosts yes` 输出一致的 `example.com` 条目（端口 22）。
    const HASHED_LINE: &str = "|1|O33ESRMWPVkMYIwJ1Uw+n877jTo=|nuuC5vEqXlEZ/8BXQR7m619W6Ak=";

    fn key(raw: &str) -> PublicKey {
        PublicKey::from_openssh(raw).expect("parse key")
    }

    #[test]
    fn literal_host_and_default_port() {
        let raw = format!("example.com {ED25519_A}\n");
        assert_eq!(
            verify_str(&raw, "example.com", 22, &key(ED25519_A)),
            HostKeyVerdict::Match
        );
        assert_eq!(
            verify_str(&raw, "other.com", 22, &key(ED25519_A)),
            HostKeyVerdict::Unknown
        );
        // 裸主机名只在 22 端口有效
        assert_eq!(
            verify_str(&raw, "example.com", 2222, &key(ED25519_A)),
            HostKeyVerdict::Unknown
        );
    }

    #[test]
    fn non_default_port_uses_bracket_form() {
        let raw = format!("[example.com]:2222 {ED25519_A}\n");
        assert_eq!(
            verify_str(&raw, "example.com", 2222, &key(ED25519_A)),
            HostKeyVerdict::Match
        );
        assert_eq!(
            verify_str(&raw, "example.com", 22, &key(ED25519_A)),
            HostKeyVerdict::Unknown
        );
    }

    #[test]
    fn changed_key_is_mismatch_with_line_number() {
        let raw = format!("# header\n\nexample.com {ED25519_A}\n");
        assert_eq!(
            verify_str(&raw, "EXAMPLE.com", 22, &key(ED25519_B)),
            HostKeyVerdict::Mismatch { line: 3 }
        );
    }

    #[test]
    fn wildcard_and_negation() {
        let raw = format!("*.example.com,!bad.example.com {ED25519_A}\n");
        assert_eq!(
            verify_str(&raw, "box.example.com", 22, &key(ED25519_A)),
            HostKeyVerdict::Match
        );
        assert_eq!(
            verify_str(&raw, "bad.example.com", 22, &key(ED25519_A)),
            HostKeyVerdict::Unknown
        );
    }

    #[test]
    fn question_mark_wildcard() {
        assert!(glob_match("192.168.1.??", "192.168.1.10"));
        assert!(!glob_match("192.168.1.??", "192.168.1.100"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b*c", "aXXbYY"));
    }

    #[test]
    fn hashed_hostname_matches() {
        let raw = format!("{HASHED_LINE} {ED25519_A}\n");
        assert_eq!(
            verify_str(&raw, "example.com", 22, &key(ED25519_A)),
            HostKeyVerdict::Match
        );
        assert_eq!(
            verify_str(&raw, "example.org", 22, &key(ED25519_A)),
            HostKeyVerdict::Unknown
        );
    }

    #[test]
    fn revoked_entry_wins() {
        let raw = format!("@revoked example.com {ED25519_A}\nexample.com {ED25519_A}\n");
        assert_eq!(
            verify_str(&raw, "example.com", 22, &key(ED25519_A)),
            HostKeyVerdict::Revoked { line: 1 }
        );
    }

    #[test]
    fn comment_and_trailing_comment_are_tolerated() {
        let raw = format!("# comment line\nexample.com {ED25519_A} root@laptop\n");
        assert_eq!(
            verify_str(&raw, "example.com", 22, &key(ED25519_A)),
            HostKeyVerdict::Match
        );
    }

    #[test]
    fn multiple_key_types_for_same_host_do_not_conflict() {
        // 同一主机不同算法的两条记录都应被视为「已知」，不能因为算法不同就判 MITM。
        let ecdsa = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBCr8/JJ1ZkU+AXqVD8cCEVy2RlZ155X/n/qQ7zhmCxOD12BsElMz87quABfu+7yT3A58hBV59GI2Xf4QULkiXwI=";
        let ecdsa_key = key(ecdsa);
        let raw = format!("example.com {ED25519_A}\nexample.com {ecdsa}\n");
        assert_eq!(
            verify_str(&raw, "example.com", 22, &ecdsa_key),
            HostKeyVerdict::Match
        );
        assert_eq!(
            verify_str(&raw, "example.com", 22, &key(ED25519_A)),
            HostKeyVerdict::Match
        );
    }

    #[test]
    fn learn_then_verify_roundtrip() {
        let dir = std::env::temp_dir().join(format!("sshdeck-kh-{}", uuid::Uuid::new_v4()));
        let path = dir.join("known_hosts");
        let k = key(ED25519_A);

        assert_eq!(
            verify_file(&path, "example.com", 2222, &k).expect("verify missing file"),
            HostKeyVerdict::Unknown
        );

        learn_file(&path, "example.com", 2222, &k).expect("learn");
        assert_eq!(
            verify_file(&path, "example.com", 2222, &k).expect("verify"),
            HostKeyVerdict::Match
        );

        // 幂等：再学一次不应追加重复行
        learn_file(&path, "example.com", 2222, &k).expect("learn twice");
        let raw = std::fs::read_to_string(&path).expect("read");
        assert_eq!(raw.lines().count(), 1, "no duplicate line: {raw}");
        assert!(raw.contains("[example.com]:2222"), "bracket form used");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn policy_parsing() {
        assert_eq!(HostKeyPolicy::parse("strict"), Some(HostKeyPolicy::Strict));
        assert_eq!(
            HostKeyPolicy::parse("accept-new"),
            Some(HostKeyPolicy::AcceptNew)
        );
        assert_eq!(HostKeyPolicy::parse(" STRICT "), Some(HostKeyPolicy::Strict));
        // 未知值必须回落到默认策略，而不是悄悄变成 insecure
        assert_eq!(HostKeyPolicy::parse("bogus"), None);
        assert_eq!(HostKeyPolicy::default(), HostKeyPolicy::Strict);
    }
}
