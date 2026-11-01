//! 命令审批审计日志（契约 `docs/FEATURE-MODEL-PROVIDERS.md` §5.3）。
//!
//! 设计要求（硬性）：
//! - **审计不能阻断运维**：所有落盘失败都静默吞掉，`record` 永不 panic、永不返回错误
//! - 追加写 JSONL（每行一个 [`AuditEntry`]），文件固定为 `audit.jsonl`
//! - 敏感命令**不做脱敏**——审计目的就是可回溯，但只存本机配置目录
//! - `tail` 读取最近 N 条；坏行（被截断/手改）跳过，不报错
//!
//! 线程安全：内部用 `Mutex` 串行化同一进程内的写入，保证行不交错。

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::ipc::dto::Risk;

/// 审计文件名（落在配置目录下）。
pub const AUDIT_FILE_NAME: &str = "audit.jsonl";

/// 审批决策 —— 与契约 §5.3 的 `decision` 字段取值一一对应。
pub mod decision {
    /// 只读命令自动执行，未经过审批。
    pub const AUTO: &str = "auto";
    /// 审批通过（`Dangerous` 时含二次确认短语匹配）。
    pub const APPROVED: &str = "approved";
    /// 用户拒绝，或二次确认短语不匹配。
    pub const REJECTED: &str = "rejected";
    /// 审批等待超时（300s），命令未执行。
    pub const TIMEOUT: &str = "timeout";
    /// 对话被用户中止，命令未执行。
    pub const ABORTED: &str = "aborted";
}

/// 一条审批审计记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Unix 毫秒时间戳。
    pub ts: i64,
    pub session_id: String,
    pub command: String,
    /// `read_only` / `mutating` / `dangerous`
    pub risk: String,
    /// `auto` / `approved` / `rejected` / `timeout` / `aborted`
    pub decision: String,
    /// 可选补充说明（分类原因、二次确认结果等）。
    pub reason: Option<String>,
}

impl AuditEntry {
    /// 用当前时间构造一条记录；`risk` 自动转成稳定字符串。
    pub fn new(
        session_id: impl Into<String>,
        command: impl Into<String>,
        risk: Risk,
        decision: impl Into<String>,
    ) -> Self {
        Self {
            ts: now_ts(),
            session_id: session_id.into(),
            command: command.into(),
            risk: risk_label(risk).to_string(),
            decision: decision.into(),
            reason: None,
        }
    }

    /// 补上 `reason`（链式风格）。
    pub fn with_reason(mut self, reason: Option<String>) -> Self {
        self.reason = reason;
        self
    }

    /// 补上显式时间戳（测试与回放用）。
    pub fn with_ts(mut self, ts: i64) -> Self {
        self.ts = ts;
        self
    }
}

/// `Risk` → 审计字符串（稳定、可 grep，不随 enum 重命名漂移）。
pub fn risk_label(risk: Risk) -> &'static str {
    match risk {
        Risk::ReadOnly => "read_only",
        Risk::Mutating => "mutating",
        Risk::Dangerous => "dangerous",
    }
}

/// Unix 毫秒时间戳；系统时钟异常（早于 epoch）时回落 0，不 panic。
pub fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 审计日志。可安全地在多个 task 间共享（`&self` 方法 + 内部 `Mutex`）。
pub struct AuditLog {
    path: PathBuf,
    /// 只保护写序列化，不保护文件句柄（每次 append 都重新打开，避免句柄泄漏）。
    write_lock: Mutex<()>,
}

impl AuditLog {
    /// 在配置目录下创建/打开审计日志（`<config_dir>/audit.jsonl`）。
    ///
    /// 不在这里创建目录/文件：真正写第一条时再创建，失败也静默。
    pub fn new(config_dir: PathBuf) -> Self {
        Self {
            path: config_dir.join(AUDIT_FILE_NAME),
            write_lock: Mutex::new(()),
        }
    }

    /// 显式指定审计文件路径（测试用）。
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            path,
            write_lock: Mutex::new(()),
        }
    }

    /// 审计文件绝对路径（供 UI/文档展示）。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 追加写一条记录。**失败静默**：审计永远不能阻断运维。
    pub fn record(&self, entry: AuditEntry) {
        // 审计不能 panic：写锁被 poison 时也继续（用 into_inner 语义兜底）
        let _guard = match self.write_lock.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };

        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() && std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }

        let line = match serde_json::to_string(&entry) {
            Ok(l) => l,
            Err(_) => return,
        };

        let mut file = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(f) => f,
            Err(_) => return,
        };
        // 一次 write_all 写完整行（含换行），降低交错风险
        let _ = file.write_all(format!("{line}\n").as_bytes());
        let _ = file.flush();
    }

    /// 便捷方法：直接记录一次决策。
    pub fn record_decision(
        &self,
        session_id: &str,
        command: &str,
        risk: Risk,
        decision: &str,
        reason: Option<String>,
    ) {
        self.record(
            AuditEntry::new(session_id, command, risk, decision).with_reason(reason),
        );
    }

    /// 读取最近 `limit` 条记录（按时间正序返回，最后一条是最近的）。
    ///
    /// - 文件不存在 → 空
    /// - 坏行（JSON 解析失败）→ 跳过，不报错
    /// - `limit == 0` → 空
    pub fn tail(&self, limit: usize) -> Vec<AuditEntry> {
        if limit == 0 {
            return Vec::new();
        }
        let file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };

        let mut entries: Vec<AuditEntry> = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue, // 读中断（例如编码问题）也跳过
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<AuditEntry>(trimmed) {
                Ok(e) => entries.push(e),
                Err(_) => continue, // 坏行跳过
            }
        }

        if entries.len() > limit {
            entries.drain(0..entries.len() - limit);
        }
        entries
    }

    /// 全量条数（用于 UI 展示「已记录 N 条」）。坏行不计入。
    pub fn count(&self) -> usize {
        self.tail(usize::MAX).len()
    }
}

/// 按 `decision` 统计条数，供审计面板做概览（`HashMap<decision, count>`）。
pub fn summarize(entries: &[AuditEntry]) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for e in entries {
        *map.entry(e.decision.clone()).or_insert(0) += 1;
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试用独立临时目录，避免并行测试互相污染。
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sshdeck-audit-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn entry(ts: i64, cmd: &str, decision: &str) -> AuditEntry {
        AuditEntry::new("sess-1", cmd, Risk::Dangerous, decision)
            .with_reason(Some("测试原因".to_string()))
            .with_ts(ts)
    }

    #[test]
    fn record_three_then_tail_roundtrips() {
        let dir = temp_dir("roundtrip");
        let log = AuditLog::new(dir.clone());
        assert_eq!(log.path(), dir.join(AUDIT_FILE_NAME));

        let a = entry(100, "rm -rf /tmp/x", decision::APPROVED);
        let b = entry(200, "ls -la", decision::AUTO);
        let c = entry(300, "reboot", decision::REJECTED);
        log.record(a.clone());
        log.record(b.clone());
        log.record(c.clone());

        // 落盘是 JSONL：一行一条，末尾有换行
        let raw = std::fs::read_to_string(log.path()).unwrap();
        assert_eq!(raw.lines().count(), 3, "应为 3 行 JSONL");
        assert!(raw.ends_with('\n'));
        assert!(raw.contains("\"risk\":\"dangerous\""), "risk 用稳定字符串");

        let got = log.tail(3);
        assert_eq!(got, vec![a, b, c], "tail(3) 读回应与写入一致");
        assert_eq!(log.count(), 3);

        // limit 小于总数时只取最近的
        let last2 = log.tail(2);
        assert_eq!(last2.len(), 2);
        assert_eq!(last2[0].command, "ls -la");
        assert_eq!(last2[1].command, "reboot");

        assert!(log.tail(0).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_lines_are_skipped() {
        let dir = temp_dir("badlines");
        let log = AuditLog::new(dir.clone());
        log.record(entry(1, "sudo -i", decision::REJECTED));

        // 手工注入坏行：非法 JSON、空行、截断的一半
        {
            let mut f = OpenOptions::new().append(true).open(log.path()).unwrap();
            f.write_all(b"not json at all\n").unwrap();
            f.write_all(b"\n").unwrap();
            f.write_all(b"{\"ts\":2,\"session_id\":\"sess-1\",\"comm").unwrap();
            f.write_all(b"\n").unwrap();
        }
        log.record(entry(3, "mkfs.ext4 /dev/sda1", decision::TIMEOUT));

        let got = log.tail(10);
        assert_eq!(got.len(), 2, "应为 2 条合法记录（坏行被跳过）");
        assert_eq!(got[0].command, "sudo -i");
        assert_eq!(got[1].command, "mkfs.ext4 /dev/sda1");
        assert_eq!(got[1].decision, "timeout");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_empty_not_error() {
        let dir = temp_dir("missing");
        let log = AuditLog::new(dir.clone());
        assert!(log.tail(5).is_empty());
        assert_eq!(log.count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn creates_parent_dir_on_demand() {
        let dir = temp_dir("nested");
        // new() 不建目录；record 时才建
        let nested = dir.join("sub/deeper");
        let log = AuditLog::new(nested);
        log.record(entry(1, "df -h", decision::AUTO));
        assert!(log.path().exists(), "record 应自动创建父目录并落盘");
        assert_eq!(log.tail(1).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn risk_labels_are_stable() {
        assert_eq!(risk_label(Risk::ReadOnly), "read_only");
        assert_eq!(risk_label(Risk::Mutating), "mutating");
        assert_eq!(risk_label(Risk::Dangerous), "dangerous");
    }

    #[test]
    fn summarize_counts_by_decision() {
        let entries = vec![
            entry(1, "a", decision::AUTO),
            entry(2, "b", decision::APPROVED),
            entry(3, "c", decision::APPROVED),
        ];
        let s = summarize(&entries);
        assert_eq!(s.get(decision::APPROVED).copied(), Some(2));
        assert_eq!(s.get(decision::AUTO).copied(), Some(1));
        assert_eq!(s.get(decision::REJECTED).copied(), None);
    }
}
