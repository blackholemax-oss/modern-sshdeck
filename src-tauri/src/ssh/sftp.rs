//! 远程文件管理（SFTP）。
//!
//! ## 设计要点
//!
//! 1. **复用已有 SSH 连接**：文件操作走当前会话的 `russh::client::Handle`，
//!    不新建 TCP 连接 —— 否则每次列目录都要重新认证，既慢又容易被服务端限流。
//! 2. **路径安全**：所有路径都做规范化，拒绝 `..` 越界到用户主目录之外的操作。
//!    这里不是「沙箱」而是**防手滑**：远程端本来就有 SSH 权限，越权拦不住，
//!    但 UI 上误点「删除上级目录」是真会出事的。
//! 3. **大文件不整读**：预览只取前 N KB，二进制文件直接拒绝预览（避免把
//!    几百 MB 的模型文件读进内存）。
//! 4. **目录列表排序**：目录在前、文件在后，各自按名称排序 —— 与常见
//!    SFTP 客户端的习惯一致。

use std::time::Duration;

use russh_sftp::client::SftpSession;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

/// 预览时最多读取的字节数（1 MB）。
///
 /// 超过就走「部分预览」并明确告知用户，而不是静默截断。
pub const PREVIEW_LIMIT: usize = 1024 * 1024;

/// 超过这个体积就不做文本预览（提示用户改用下载）。
pub const MAX_PREVIEW_FILE: u64 = 8 * 1024 * 1024;

/// 一个目录项。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEntry {
    pub name: String,
    /// 完整路径（已规范化）
    pub path: String,
    /// `file` / `dir` / `symlink` / `other`
    pub kind: String,
    pub size: u64,
    /// 修改时间（Unix 秒）；未知为 0
    pub modified: u64,
    /// 权限位（如 `0o644`）；未知为 0
    pub mode: u32,
}

impl RemoteEntry {
    fn is_dir(&self) -> bool {
        self.kind == "dir"
    }
}

/// 目录列表结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirListing {
    /// 已规范化的当前目录
    pub path: String,
    /// 上一级（已在根目录时为 `None`）
    pub parent: Option<String>,
    pub entries: Vec<RemoteEntry>,
}

/// 文件预览结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePreview {
    pub path: String,
    pub size: u64,
    /// 是否为文本（`content` 才有意义）
    pub is_text: bool,
    /// 文本内容；二进制或超大文件为 `None`
    pub content: Option<String>,
    /// 内容是否被截断（只读了前 [`PREVIEW_LIMIT`] 字节）
    pub truncated: bool,
    /// 无法预览时的原因（给用户看的）
    pub reason: Option<String>,
}

/// 规范化远程路径。
///
 /// 规则：
 /// - 展开 `~` 为 `/`（远程 SFTP 的 `~` 语义由服务端决定，这里保守按根处理，
 ///   真正的家目录由 `canonicalize` 的结果决定）
 /// - 折叠重复斜杠、去掉尾随斜杠
 /// - **拒绝** `..` 上溯（返回错误而不是悄悄改成别处）
 pub fn normalize_path(raw: &str) -> AppResult<String> {
    let p = raw.trim();
    if p.is_empty() {
        return Err(AppError::InvalidInput("路径不能为空".into()));
    }
    // 拒绝 `..`：UI 不应产生它，出现即说明调用方有问题
    if p.split('/').any(|seg| seg == "..") {
        return Err(AppError::InvalidInput(format!(
            "路径不允许包含 `..`（收到 {raw:?}）"
        )));
    }
    let cleaned = p.trim_start_matches('/').trim_end_matches('/');
    if cleaned.is_empty() {
        return Ok("/".to_string());
    }
    // 折叠重复斜杠
    let parts: Vec<&str> = cleaned.split('/').filter(|s| !s.is_empty()).collect();
    Ok(format!("/{}", parts.join("/")))
}

/// 由规范化路径算父目录。
 pub fn parent_of(path: &str) -> Option<String> {
    let p = path.trim_end_matches('/');
    if p.is_empty() || p == "/" {
        return None;
    }
    match p.rfind('/') {
        Some(0) => Some("/".to_string()),
        Some(i) => Some(p[..i].to_string()),
        None => None,
    }
}

/// 判断内容是否像文本（按是否有 NUL 字节 + 不可打印字符比例）。
///
 /// 只看前 512 字节：足够判断，且不会为了判断而多读。
 pub fn looks_like_text(bytes: &[u8]) -> bool {
    // 有 NUL 基本可断定是二进制
    if bytes.iter().any(|b| *b == 0) {
        return false;
    }
    let sample = &bytes[..bytes.len().min(512)];
    if sample.is_empty() {
        return true; // 空文件按文本处理
    }
    let printable = sample
        .iter()
        .filter(|b| {
            let c = **b;
            // 允许 ASCII 可见字符、换行、回车、制表符，以及 >=0x80 的 UTF-8 字节
            (0x20..=0x7e).contains(&c) || c == b'\n' || c == b'\r' || c == b'\t' || c >= 0x80
        })
        .count();
    printable * 100 / sample.len() >= 95
}

/// 打开一个 SFTP 会话。
///
 /// 由调用方传入已连接的 SSH handle；本函数只做「开 channel → 建 SFTP 会话」。
 pub async fn open_sftp(handle: &russh::client::Handle<crate::ssh::session::ClientHandler>) -> AppResult<SftpSession> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| AppError::Ssh(format!("打开 SFTP 通道失败：{e}")))?;

    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| AppError::Ssh(format!("请求 sftp 子系统失败：{e}")))?;

    let sftp = SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| AppError::Ssh(format!("建立 SFTP 会话失败：{e}")))?;

    Ok(sftp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_handles_root_and_slashes() {
        assert_eq!(normalize_path("/").unwrap(), "/");
        assert_eq!(normalize_path("").is_err(), true);
        assert_eq!(normalize_path("/a/b").unwrap(), "/a/b");
        assert_eq!(normalize_path("/a//b///c").unwrap(), "/a/b/c");
        assert_eq!(normalize_path("a/b/").unwrap(), "/a/b");
    }

    #[test]
    fn normalize_rejects_dotdot() {
        // `..` 会让 UI 操作跑到预期之外的目录，必须拒绝
        assert!(normalize_path("/a/../b").is_err());
        assert!(normalize_path("../x").is_err());
        assert!(normalize_path("/..").is_err());
    }

    #[test]
    fn parent_of_is_consistent() {
        assert_eq!(parent_of("/a/b"), Some("/a".to_string()));
        assert_eq!(parent_of("/a"), Some("/".to_string()));
        assert_eq!(parent_of("/"), None);
    }

    #[test]
    fn text_detection_basics() {
        assert!(looks_like_text(b"hello world\n"));
        assert!(looks_like_text(b""));
        assert!(looks_like_text("中文内容".as_bytes()));
        // 含 NUL 的一律当二进制
        assert!(!looks_like_text(b"\x00\x01\x02"));
        // 大量不可打印字符 → 二进制
        let bin: Vec<u8> = (0u8..32).collect();
        assert!(!looks_like_text(&bin));
    }

    #[test]
    fn entry_kind_helpers() {
        let d = RemoteEntry {
            name: "x".into(),
            path: "/x".into(),
            kind: "dir".into(),
            size: 0,
            modified: 0,
            mode: 0,
        };
        assert!(d.is_dir());
    }
}
