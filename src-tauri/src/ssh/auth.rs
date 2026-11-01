//! SSH 认证。
//!
//! 契约：
//! - 密码认证
//! - 私钥认证（OpenSSH 格式 / PEM / PuTTY PPK，支持 passphrase）
//! - 私钥路径支持 `~` 展开
//!
//! 说明：russh 0.54 已把 `russh-keys` 合并进门面 `russh::keys`（重导出 `ssh-key`），
//! 因此这里统一使用 `russh::keys::*`，不再依赖独立的 `russh-keys` crate。

use std::path::PathBuf;
use std::sync::Arc;

use russh::keys::{decode_secret_key, HashAlg, PrivateKey, PrivateKeyWithHashAlg};

use crate::error::{AppError, AppResult};

/// 展开 `~` 为当前用户 home 目录。
pub fn expand_path(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

/// 读取私钥文件内容。
pub fn read_key(path: &str) -> AppResult<String> {
    let p = expand_path(path);
    std::fs::read_to_string(&p)
        .map_err(|e| AppError::Ssh(format!("read private key {}: {e}", p.display())))
}

/// 读取并（必要时）解密私钥。
///
/// - 未加密私钥：直接解析
/// - 加密私钥：必须有 `passphrase`，否则返回明确可读的 `AppError::Ssh`
///
/// 支持的格式由 russh/ssh-key 决定：OpenSSH 新格式、PKCS#8/PKCS#1 PEM、PuTTY PPK。
pub fn load_private_key(path: &str, passphrase: Option<&str>) -> AppResult<Arc<PrivateKey>> {
    let pem = read_key(path)?;
    // 空字符串等价于"没有 passphrase"，避免把 "" 当成密码传给解密逻辑。
    let pass = passphrase.map(str::trim).filter(|s| !s.is_empty());

    match decode_secret_key(&pem, pass) {
        Ok(key) => Ok(Arc::new(key)),
        Err(e) => {
            let msg = e.to_string();
            let lower = msg.to_ascii_lowercase();
            if pass.is_none() && (lower.contains("encrypt") || lower.contains("password")) {
                Err(AppError::Ssh(format!(
                    "private key {} is passphrase-protected: passphrase is required ({msg})",
                    expand_path(path).display()
                )))
            } else if pass.is_some() && (lower.contains("encrypt") || lower.contains("password")) {
                Err(AppError::Ssh(format!(
                    "cannot decrypt private key {}: wrong passphrase ({msg})",
                    expand_path(path).display()
                )))
            } else {
                Err(AppError::Ssh(format!(
                    "parse private key {}: {msg}",
                    expand_path(path).display()
                )))
            }
        }
    }
}

/// 构造认证用密钥包装（RSA 需要 hash 算法，非 RSA 密钥会忽略该参数）。
pub fn with_hash_alg(key: Arc<PrivateKey>, hash_alg: Option<HashAlg>) -> PrivateKeyWithHashAlg {
    PrivateKeyWithHashAlg::new(key, hash_alg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home_prefix() {
        let p = expand_path("~/.ssh/id_ed25519");
        assert!(p.ends_with("id_ed25519"));
    }

    #[test]
    fn keeps_absolute_path() {
        let p = expand_path("C:/keys/id_rsa");
        assert_eq!(p, PathBuf::from("C:/keys/id_rsa"));
    }
}
