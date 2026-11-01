//! 远程文件管理的 IPC 命令（SFTP）。
//!
//! 所有命令都带 `session_id`：文件操作绑定在**已有 SSH 会话**上，
//! 不新建连接（详见 `ssh::sftp` 的模块注释）。

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::ssh::sftp::{DirListing, FilePreview, RemoteEntry};
use crate::ssh::sftp::{self, MAX_PREVIEW_FILE, PREVIEW_LIMIT};

/// 列目录。`path` 为空时用 `/`。
#[tauri::command]
pub async fn fs_list_dir(
    state: State<'_, crate::state::AppState>,
    session_id: String,
    path: String,
) -> AppResult<DirListing> {
    let dir = sftp::normalize_path(if path.trim().is_empty() { "/" } else { &path })?;
    let sftp_session = state.sessions.open_sftp(&session_id).await?;

    // 先 canonicalize：把 ~ 之类交给服务端解析，得到真实绝对路径
    let real = sftp_session
        .canonicalize(dir.clone())
        .await
        .map_err(|e| AppError::Ssh(format!("无法解析目录 {dir}: {e}")))?;

    let mut entries = Vec::new();
    let read = sftp_session
        .read_dir(real.clone())
        .await
        .map_err(|e| AppError::Ssh(format!("读取目录 {real} 失败：{e}")))?;

    for item in read {
        let name = item.file_name().to_string();
        // `russh-sftp` 的 metadata() 直接返回 FileAttributes（不是 Result）
        let meta = item.metadata();
        let kind = if meta.is_dir() {
            "dir"
        } else if meta.is_symlink() {
            "symlink"
        } else if meta.is_regular() {
            "file"
        } else {
            "other"
        };

        entries.push(RemoteEntry {
            path: format!("{}/{}", real.trim_end_matches('/'), name),
            name,
            kind: kind.to_string(),
            size: meta.len(),
            modified: meta.mtime.unwrap_or(0) as u64,
            mode: meta.permissions.unwrap_or(0),
        });
    }

    // 目录在前、文件在后，各自按名称排序（与常见 SFTP 客户端一致）
    entries.sort_by(|a, b| {
        let ad = a.kind == "dir";
        let bd = b.kind == "dir";
        bd.cmp(&ad).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(DirListing {
        parent: sftp::parent_of(&real),
        path: real,
        entries,
    })
}

/// 预览文件。
///
 /// 只读前 [`PREVIEW_LIMIT`] 字节；二进制或超大文件不返回内容而是给出原因，
 /// 避免把大文件读进内存或让 UI 渲染乱码。
#[tauri::command]
pub async fn fs_read_file(
    state: State<'_, crate::state::AppState>,
    session_id: String,
    path: String,
) -> AppResult<FilePreview> {
    let p = sftp::normalize_path(&path)?;
    let sftp_session = state.sessions.open_sftp(&session_id).await?;

    let size = sftp_session
        .metadata(p.clone())
        .await
        .map(|m| m.len())
        .map_err(|e| AppError::Ssh(format!("读取 {p} 的元信息失败：{e}")))?;

    if size > MAX_PREVIEW_FILE {
        return Ok(FilePreview {
            path: p,
            size,
            is_text: false,
            content: None,
            truncated: false,
            reason: Some(format!(
                "文件 {:.1} MB，超过预览上限 {:.0} MB，请改用下载",
                size as f64 / 1_048_576.0,
                MAX_PREVIEW_FILE as f64 / 1_048_576.0
            )),
        });
    }

    let data = sftp_session
        .read(p.clone())
        .await
        .map_err(|e| AppError::Ssh(format!("读取 {p} 失败：{e}")))?;

    let truncated = data.len() > PREVIEW_LIMIT;
    let head = &data[..data.len().min(PREVIEW_LIMIT)];

    if !sftp::looks_like_text(head) {
        return Ok(FilePreview {
            path: p,
            size: data.len() as u64,
            is_text: false,
            content: None,
            truncated: false,
            reason: Some("二进制文件，无法以文本预览".to_string()),
        });
    }

    Ok(FilePreview {
        path: p,
        size: data.len() as u64,
        is_text: true,
        content: Some(String::from_utf8_lossy(head).into_owned()),
        truncated,
        reason: if truncated {
            Some(format!(
                "已截断：只显示前 {} 字节",
                PREVIEW_LIMIT
            ))
        } else {
            None
        },
    })
}

/// 下载文件到本机（返回本地路径）。
#[tauri::command]
pub async fn fs_download(
    state: State<'_, crate::state::AppState>,
    session_id: String,
    remote_path: String,
    local_path: String,
) -> AppResult<u64> {
    let p = sftp::normalize_path(&remote_path)?;
    let sftp_session = state.sessions.open_sftp(&session_id).await?;
    let data = sftp_session
        .read(p.clone())
        .await
        .map_err(|e| AppError::Ssh(format!("读取 {p} 失败：{e}")))?;
    let n = data.len() as u64;
    std::fs::write(&local_path, &data)
        .map_err(|e| AppError::InvalidInput(format!("写入本地文件 {local_path} 失败：{e}")))?;
    Ok(n)
}

/// 上传本地文件到远端。
#[tauri::command]
pub async fn fs_upload(
    state: State<'_, crate::state::AppState>,
    session_id: String,
    local_path: String,
    remote_path: String,
) -> AppResult<u64> {
    let p = sftp::normalize_path(&remote_path)?;
    let data = std::fs::read(&local_path)
        .map_err(|e| AppError::InvalidInput(format!("读取本地文件 {local_path} 失败：{e}")))?;
    let n = data.len() as u64;
    let sftp_session = state.sessions.open_sftp(&session_id).await?;
    sftp_session
        .write(p.clone(), &data)
        .await
        .map_err(|e| AppError::Ssh(format!("写入 {p} 失败：{e}")))?;
    Ok(n)
}
