//! RTK（Read-Token-Kit）—— 工具输出压缩器。
//!
//! 契约：`docs/FEATURE-MODEL-PROVIDERS.md` §4。本模块是**纯函数**实现，不做 IO、
//! 不依赖网络、不改变语义顺序；六个过滤器按优先级顺序尝试，第一个「安全且确实变小」
//! 的结果胜出，否则原样放行。
//!
//! ## §4.2 五条安全规则（本模块的硬性不变量）
//!
//! 1. 报错输出永不压缩（`looks_like_error`）。
//! 2. 小于 `MIN_BYTES = 2048` 不压缩。
//! 3. 压缩结果永不大于原文（`final_text.len() < raw.len()` 才接受，否则回退）。
//! 4. 幂等：输出带压缩标记行，二次调用立即放行（`is_compressed`）。
//! 5. 失败即放行：过滤器返回 `None` 或 panic 都回退原文（`catch_unwind` 兜底）。

use std::panic::{catch_unwind, AssertUnwindSafe};

/// 小于该字节数的输出一律不压缩（§4.2 规则 2）。
pub const MIN_BYTES: usize = 2048;

/// 压缩标记的前缀；命中该前缀即视为「已压缩」（§4.2 规则 4 幂等判据）。
pub const MARKER_PREFIX: &str = "[已压缩：";

/// 输出里只出现弱错误词（error/failed…）时，短于此值即放行。
const ERROR_SOFT_BYTES: usize = 8 * 1024;

/// 未变更上下文的保留行数（hunk 内每个变更点前后各留 N 行）。
const CTX_KEEP: usize = 3;
/// 单个 diff 最多保留的变更行数；超过则抽稀（§4.3「超长时只留前后各 N 行」）。
const DIFF_CHANGED_CAP: usize = 60;
/// git status 每一类保留的路径条数。
const STATUS_KEEP: usize = 6;
/// git log 保留的完整提交条数，以及摘要区最多列出的条数。
const LOG_KEEP: usize = 4;
const LOG_SUMMARY_CAP: usize = 20;
/// grep 命中：每个文件保留的命中行数，以及最多聚合的文件数。
const GREP_KEEP: usize = 6;
const GREP_FILES_CAP: usize = 24;
/// 文件列表保留的前 N 行。
const LIST_KEEP: usize = 40;
/// 相邻重复行折叠的最小连续次数。
const REPEAT_MIN_RUN: usize = 3;

/// 压缩结果。`text` 为空时表示「没有可用结果」，但本模块始终返回原文或压缩文。
#[derive(Debug, Clone)]
pub struct CompressOutcome {
    /// 压缩后（或原样）文本。
    pub text: String,
    /// 原始字节数。
    pub before_bytes: u64,
    /// 输出字节数。
    pub after_bytes: u64,
    /// 是否真的压缩了（而非放行原文）。
    pub applied: bool,
    /// 命中的过滤器名；放行时为 `None`。
    pub filter: Option<&'static str>,
}

impl CompressOutcome {
    /// 节省的字节数（放行时为 0）。
    pub fn saved_bytes(&self) -> u64 {
        self.before_bytes.saturating_sub(self.after_bytes)
    }
}

/// 单个过滤器的产物：折叠后的正文 + 被省略的行数 + 回查提示。
struct Candidate {
    body: String,
    omitted_lines: usize,
    hint: String,
}

type FilterFn = fn(&str) -> Option<Candidate>;

/// 过滤器表，顺序即优先级（§4.3）。
const FILTERS: &[(&str, FilterFn)] = &[
    ("unified_diff", unified_diff),
    ("git_status", git_status),
    ("git_log", git_log),
    ("grep_result", grep_result),
    ("file_list", file_list),
    ("repeat_lines", repeat_lines),
];

/// 压缩一段工具输出。
///
/// 任何情况下都返回一段可直接回填给模型的文本：要么是压缩视图（末尾带压缩标记），
/// 要么是原始文本（`applied == false`）。
pub fn compress_tool_output(raw: &str) -> CompressOutcome {
    let before = raw.len();
    let pass = |text: &str| CompressOutcome {
        text: text.to_string(),
        before_bytes: before as u64,
        after_bytes: text.len() as u64,
        applied: false,
        filter: None,
    };

    // 规则 2：小输出不压缩。
    if before < MIN_BYTES {
        return pass(raw);
    }
    // 规则 4：已经带压缩标记 → 直接放行，保证 compress(compress(x)) == compress(x)。
    if is_compressed(raw) {
        return pass(raw);
    }
    // 规则 1：报错输出不压缩。
    if looks_like_error(raw) {
        return pass(raw);
    }

    for (name, filter) in FILTERS {
        // 规则 5：过滤器 panic / 出错即放行，继续尝试下一个过滤器。
        let candidate = match catch_unwind(AssertUnwindSafe(|| filter(raw))) {
            Ok(Some(c)) => c,
            _ => continue,
        };
        if candidate.omitted_lines == 0 {
            continue;
        }
        let final_text = finalize(&candidate, before);
        // 规则 3：压缩结果不能更大（也不接受「只小一点点」的噪声级收益）。
        if final_text.len() >= before {
            continue;
        }
        // 小于 5% 的收益视为噪声，不值得引入压缩标记。
        if before - final_text.len() < before / 20 {
            continue;
        }
        let after = final_text.len();
        return CompressOutcome {
            text: final_text,
            before_bytes: before as u64,
            after_bytes: after as u64,
            applied: true,
            filter: Some(name),
        };
    }

    pass(raw)
}

/// 拼接正文 + 压缩标记（§4.4）。标记里的大小信息包含标记自身，迭代两次收敛。
fn finalize(c: &Candidate, before: usize) -> String {
    let body = c.body.trim_end_matches('\n');
    let mut guess = body.len() + 80;
    let mut text = String::new();
    for _ in 0..3 {
        let marker = build_marker(before, guess, c.omitted_lines, &c.hint);
        text = format!("{body}\n{marker}");
        if text.len() == guess {
            break;
        }
        guess = text.len();
    }
    text
}

/// §4.4 压缩标记：
/// `[已压缩：原始 40.2 KB → 3.9 KB，省略 214 行。需要完整内容请执行 `git diff`]`
fn build_marker(before: usize, after: usize, omitted: usize, hint: &str) -> String {
    format!(
        "{MARKER_PREFIX}原始 {} → {}，省略 {} 行。需要完整内容请执行 `{}`]",
        human_bytes(before),
        human_bytes(after),
        omitted,
        hint
    )
}

fn human_bytes(n: usize) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

/// 是否已经是压缩视图（幂等判据）。
pub fn is_compressed(raw: &str) -> bool {
    raw.lines()
        .any(|l| l.trim_start().starts_with(MARKER_PREFIX))
}

/// §4.2 规则 1：报错输出不压缩。
///
/// 判定分三层，避免「正文里恰好出现 error 字样」的良性大输出被误判放行：
/// 1. 首行是错误语气 → 直接放行（求知型错误标记开头）。
/// 2. 强错误标记出现在任意位置（panic / Traceback / fatal: …）→ 放行。
/// 3. 短输出（< `ERROR_SOFT_BYTES`）且前 20 个非空行里有 ≥2 行错误语气 → 放行。
fn looks_like_error(raw: &str) -> bool {
    // 强信号：无论多长都放行 —— 隐藏报错比多花 token 危险得多。
    const HARD: [&str; 6] = [
        "panicked at",
        "Traceback (most recent call last)",
        "fatal: ",
        "panic: ",
        "stack backtrace:",
        "Segmentation fault",
    ];
    if HARD.iter().any(|p| raw.contains(p)) {
        return true;
    }

    let first = raw.lines().find(|l| !l.trim().is_empty());
    if first.map(is_error_line).unwrap_or(false) {
        return true;
    }

    // 弱信号：只在较短输出里生效；要求前 20 个非空行至少有 2 行错误语气，
    // 这样「大 diff 里恰好有一行 `+ eprintln!("error: …")`」不会被误判。
    if raw.len() >= ERROR_SOFT_BYTES {
        return false;
    }
    let err_lines = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(20)
        .filter(|l| is_error_line(l))
        .count();
    err_lines >= 2
}

/// 单行是否带错误语气。
fn is_error_line(line: &str) -> bool {
    let t = line.trim_start();
    let t = t.trim_start_matches(|c: char| "×✖✗!+-".contains(c));
    let t = t.trim_start();
    const PREFIXES: [&str; 10] = [
        "error",
        "ERROR",
        "Error",
        "fatal",
        "FATAL",
        "failed",
        "FAILED",
        "panic",
        "Cannot",
        "cannot",
    ];
    if PREFIXES.iter().any(|p| t.starts_with(p)) {
        return true;
    }
    const CONTAINS: [&str; 5] = [
        "Traceback",
        "npm ERR!",
        "No such file",
        "Permission denied",
        "command not found",
    ];
    CONTAINS.iter().any(|p| t.contains(p))
}

/// 输出行的「未变更上下文」判定：以空格开头（unified diff 的 context 行）。
fn is_change_line(line: &str) -> bool {
    matches!(line.chars().next(), Some('+') | Some('-'))
}

fn is_hunk_header(line: &str) -> bool {
    line.starts_with("@@ ") || line.starts_with("@@-") || line.starts_with("@@ -")
}

fn is_file_header(line: &str) -> bool {
    line.starts_with("diff --git ")
        || line.starts_with("diff -")
        || line.starts_with("index ")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with("new file mode ")
        || line.starts_with("deleted file mode ")
        || line.starts_with("old mode ")
        || line.starts_with("new mode ")
        || line.starts_with("similarity index ")
        || line.starts_with("dissimilarity index ")
        || line.starts_with("rename from ")
        || line.starts_with("rename to ")
        || line.starts_with("copy from ")
        || line.starts_with("copy to ")
        || line == "GIT binary patch"
        || line.starts_with("Binary files ")
}

// ---------------------------------------------------------------------------
// 过滤器 1：unified diff
// ---------------------------------------------------------------------------

/// 抽稀一组变更行索引到 `budget` 条：保留头部 1/3、尾部 1/3，其余等步长取样。
fn thin_changed(changed: &[usize], budget: usize) -> Vec<usize> {
    if changed.len() <= budget || budget == 0 {
        return if budget == 0 { Vec::new() } else { changed.to_vec() };
    }
    let head = (budget / 3).max(1);
    let tail = (budget / 3).max(1);
    if head + tail >= budget {
        // budget 很小时只保留头尾。
        let mut v: Vec<usize> = changed[..head.min(changed.len())].to_vec();
        v.extend_from_slice(&changed[changed.len() - tail.min(changed.len())..]);
        return v;
    }
    let mid_budget = budget - head - tail;
    let mid_total = changed.len() - head - tail;
    let step = (mid_total / mid_budget.max(1)).max(1);
    let mut v: Vec<usize> = changed[..head].to_vec();
    for (idx, &ci) in changed[head..changed.len() - tail].iter().enumerate() {
        if idx % step == 0 {
            v.push(ci);
        }
    }
    v.extend_from_slice(&changed[changed.len() - tail..]);
    v
}

/// diff 的组成单元：原样保留的文本行，或一个已切分的 hunk。
enum Seg<'a> {
    Text(&'a str),
    Hunk { header: &'a str, body: &'a [&'a str] },
}

/// 保留文件头与 hunk 头、以及 +/- 行；每个变更点前后只留 `CTX_KEEP` 行上下文，
/// 其余连续上下文折叠为一行。变更行总量超过 `DIFF_CHANGED_CAP` 时**全局**按比例
/// 抽稀（§4.3「超长时只留前后各 N 行」），保证「全是变更、无上下文」的巨型 diff
/// 同样被压缩，同时保留每个 hunk 的首尾可读性。**输出保持原文的段落顺序**。
fn unified_diff(raw: &str) -> Option<Candidate> {
    let lines: Vec<&str> = raw.split('\n').collect();
    if lines.iter().filter(|l| is_hunk_header(l)).count() == 0 {
        return None; // 只有文件头、没有 hunk 正文时无可折叠
    }
    if !lines.iter().any(|l| is_change_line(l) && !is_file_header(l)) {
        return None;
    }

    // ---- 第一遍：按原顺序切成 Text / Hunk 段 ----
    let mut segs: Vec<Seg> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        if is_hunk_header(lines[i]) {
            let header = lines[i];
            let start = i + 1;
            let mut j = start;
            while j < lines.len() && !is_hunk_header(lines[j]) && !is_file_header(lines[j]) {
                j += 1;
            }
            segs.push(Seg::Hunk { header, body: &lines[start..j] });
            i = j;
        } else {
            segs.push(Seg::Text(lines[i]));
            i += 1;
        }
    }

    // ---- 全局预算：变更行总量超过上限时按比例抽稀 ----
    fn changed_count(body: &[&str]) -> usize {
        body.iter().filter(|l| is_change_line(l)).count()
    }
    let total_changed: usize = segs
        .iter()
        .map(|sg| match sg {
            Seg::Hunk { body, .. } => changed_count(body),
            Seg::Text(_) => 0,
        })
        .sum();
    let capped = total_changed > DIFF_CHANGED_CAP;

    // ---- 第二遍：按原顺序渲染 ----
    let mut out: Vec<String> = Vec::new();
    let mut omitted = 0usize;
    let mut omitted_changed = 0usize;
    let mut seen_hunk = false;

    for sg in &segs {
        let Seg::Hunk { header, body } = sg else {
            match sg {
                Seg::Text(t) => out.push(t.trim_end().to_string()),
                Seg::Hunk { .. } => unreachable!(),
            }
            continue;
        };
        seen_hunk = true;

        let changed: Vec<usize> = body
            .iter()
            .enumerate()
            .filter(|(_, l)| is_change_line(*l))
            .map(|(k, _)| k)
            .collect();

        // 按全局占比分配本 hunk 的变更行额度（至少 2 行，保留首尾）。
        let budget = if capped && total_changed > 0 {
            ((DIFF_CHANGED_CAP as f64 * changed.len() as f64) / total_changed as f64)
                .round()
                .max(2.0) as usize
        } else {
            changed.len()
        };
        let kept = thin_changed(&changed, budget);

        let mut keep = vec![false; body.len()];
        for (k, l) in body.iter().enumerate() {
            match l.chars().next() {
                Some('+') | Some('-') => {}
                Some('\\') => keep[k] = true,
                Some(' ') => {}
                _ => keep[k] = true,
            }
        }
        for &ci in &kept {
            keep[ci] = true;
            // 上下文展宽只纳入真正的上下文行；否则密集变更块里 ±CTX_KEEP 会把刚
            // 抽稀掉的变更行又装回来，等于没抽稀。
            let lo = ci.saturating_sub(CTX_KEEP);
            let hi = (ci + CTX_KEEP).min(body.len().saturating_sub(1));
            for k in lo..=hi {
                if body[k].starts_with(' ') {
                    keep[k] = true;
                }
            }
        }
        if !body.is_empty() && body[0].starts_with(' ') {
            keep[0] = true;
        }
        if changed.is_empty() {
            let n = body.len();
            for k in 0..n {
                if k < CTX_KEEP || k + CTX_KEEP >= n {
                    keep[k] = true;
                }
            }
        } else if kept.len() < changed.len() {
            omitted_changed += changed.len() - kept.len();
        }

        out.push(header.trim_end().to_string());
        let mut k = 0usize;
        while k < body.len() {
            if keep[k] {
                out.push(body[k].trim_end().to_string());
                k += 1;
            } else {
                let mut run = 0usize;
                while k < body.len() && !keep[k] {
                    run += 1;
                    k += 1;
                }
                if run > 0 {
                    out.push(format!("[… 省略 {run} 行未变更上下文 …]"));
                    omitted += run;
                }
            }
        }
    }

    if !seen_hunk || omitted + omitted_changed < 8 {
        return None;
    }
    while out.last().map(|l| l.is_empty()).unwrap_or(false) {
        out.pop();
    }
    if omitted_changed > 0 {
        out.push(format!("[… 另有 {omitted_changed} 行变更因超长被抽稀省略 …]"));
    }
    Some(Candidate {
        body: out.join("\n"),
        omitted_lines: omitted + omitted_changed,
        hint: "git diff".to_string(),
    })
}

// ---------------------------------------------------------------------------
// 过滤器 2：git status
// ---------------------------------------------------------------------------

/// 短格式（`git status -s`）的两位状态码 → 中文标签。
fn short_code_label(x: char, y: char) -> Option<&'static str> {
    if x == '?' && y == '?' {
        return Some("未跟踪");
    }
    if x == '!' && y == '!' {
        return Some("已忽略");
    }
    if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
        return Some("冲突未解决");
    }
    if x == 'T' || y == 'T' {
        return Some("类型变更");
    }
    if "AMDRC".contains(x) && (y == 'M' || y == 'D') {
        return Some("已暂存且未暂存");
    }
    if "AMDRC".contains(x) {
        return Some("已暂存");
    }
    if "MD".contains(y) {
        return Some("未暂存修改");
    }
    None
}

/// 解析一行 `git status -s`：前两个字符是状态码，其后必须跟空白，再接路径。
fn classify_short_line(line: &str) -> Option<(&'static str, String)> {
    let mut chars = line.chars();
    let x = chars.next()?;
    let y = chars.next()?;
    let rest = chars.as_str();
    if !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    if !"AMDRCUTX?! ".contains(x) || !"AMDRCUTX?! ".contains(y) {
        return None;
    }
    let path = rest.trim_start();
    if path.is_empty() {
        return None;
    }
    short_code_label(x, y).map(|label| (label, path.to_string()))
}

/// 长格式（`git status`）的 section 头 → 标签。
fn long_section_label(line: &str) -> Option<&'static str> {
    if line.contains("Changes not staged for commit") {
        Some("未暂存修改")
    } else if line.contains("Changes to be committed") {
        Some("已暂存")
    } else if line.contains("Untracked files") {
        Some("未跟踪")
    } else if line.contains("Unmerged paths") {
        Some("冲突未解决")
    } else if line.contains("Ignored files") {
        Some("已忽略")
    } else {
        None
    }
}

fn strip_long_prefix(t: &str) -> &str {
    if let Some(pos) = t.find(':') {
        let head = &t[..pos];
        if pos <= 24
            && !head.is_empty()
            && head
                .chars()
                .all(|c| c.is_ascii_alphabetic() || c == ' ' || c == '_')
        {
            return t[pos + 1..].trim();
        }
    }
    t
}

fn git_status(raw: &str) -> Option<Candidate> {
    let lines: Vec<&str> = raw.split('\n').collect();
    let has_long = lines.iter().any(|l| long_section_label(l).is_some())
        || lines.iter().any(|l| l.starts_with("On branch "));
    let short_count = lines
        .iter()
        .filter(|l| classify_short_line(l).is_some())
        .count();
    if !has_long && short_count < 8 {
        return None;
    }

    let mut headers: Vec<String> = Vec::new();
    let mut groups: Vec<(&'static str, Vec<String>)> = Vec::new();
    let mut current: Option<&'static str> = None;

    for line in &lines {
        let trimmed_end = line.trim_end();
        if let Some(label) = long_section_label(trimmed_end) {
            current = Some(label);
            headers.push(trimmed_end.trim().to_string());
            continue;
        }
        if trimmed_end.starts_with("On branch ")
            || trimmed_end.starts_with("Your branch ")
            || trimmed_end.starts_with("HEAD detached")
            || trimmed_end.starts_with("rebase in progress")
            || trimmed_end.starts_with("## ")
            || trimmed_end.starts_with("no changes added")
            || trimmed_end.starts_with("nothing to commit")
        {
            headers.push(trimmed_end.trim().to_string());
            continue;
        }

        if let Some(label) = current {
            // 长格式的条目：缩进 ≥ 4 个空格。
            let indented = line.starts_with("        ") || line.starts_with('\t');
            if indented && !line.trim().is_empty() {
                let path = strip_long_prefix(line.trim()).to_string();
                push_group(&mut groups, label, path);
                continue;
            }
        }
        if let Some((label, path)) = classify_short_line(line) {
            push_group(&mut groups, label, path);
        }
    }

    let total: usize = groups.iter().map(|(_, v)| v.len()).sum();
    if total < 8 {
        return None;
    }

    let mut out: Vec<String> = Vec::new();
    out.push(format!("## git status 摘要（共 {total} 个文件）"));
    for h in &headers {
        out.push(h.clone());
    }
    let mut omitted = 0usize;
    for (label, paths) in &groups {
        out.push(format!("{label}（{} 个）：", paths.len()));
        for p in paths.iter().take(STATUS_KEEP) {
            out.push(format!("  {}", p));
        }
        if paths.len() > STATUS_KEEP {
            let rest = paths.len() - STATUS_KEEP;
            out.push(format!("  … 其余 {rest} 个已省略"));
            omitted += rest;
        }
    }

    if omitted < 5 {
        return None;
    }
    Some(Candidate {
        body: out.join("\n"),
        omitted_lines: omitted,
        hint: "git status".to_string(),
    })
}

fn push_group<'a>(groups: &mut Vec<(&'static str, Vec<String>)>, label: &'static str, path: String) {
    if let Some((_, v)) = groups.iter_mut().find(|(l, _)| *l == label) {
        v.push(path);
    } else {
        groups.push((label, vec![path]));
    }
}

// ---------------------------------------------------------------------------
// 过滤器 3：git log
// ---------------------------------------------------------------------------

fn is_commit_header(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("commit ")?;
    let sha = rest.split_whitespace().next()?;
    if sha.len() >= 7 && sha.len() <= 64 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(sha)
    } else {
        None
    }
}

/// `git log --oneline` 形态：`<sha7+> <subject>`，且 sha 全为十六进制。
fn is_oneline(line: &str) -> Option<(&str, &str)> {
    let mut it = line.splitn(2, ' ');
    let sha = it.next()?;
    let subject = it.next()?.trim();
    if sha.len() >= 7
        && sha.len() <= 40
        && sha.chars().all(|c| c.is_ascii_hexdigit())
        && !subject.is_empty()
    {
        Some((sha, subject))
    } else {
        None
    }
}

fn git_log(raw: &str) -> Option<Candidate> {
    let lines: Vec<&str> = raw.split('\n').collect();
    let mut starts: Vec<usize> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if is_commit_header(l).is_some() {
            starts.push(i);
        }
    }

    // 形态 A：完整 git log（commit <sha> 块）。
    if starts.len() >= 4 {
        let blocks: Vec<&[&str]> = starts
            .iter()
            .enumerate()
            .map(|(n, &s)| {
                let end = starts.get(n + 1).copied().unwrap_or(lines.len());
                &lines[s..end]
            })
            .collect();

        let mut out: Vec<String> = Vec::new();
        let mut omitted = 0usize;
        for (n, block) in blocks.iter().enumerate() {
            if n < LOG_KEEP {
                for l in block.iter() {
                    if l.is_empty() {
                        continue;
                    }
                    out.push(l.trim_end().to_string());
                }
                if n + 1 < blocks.len() {
                    out.push(String::new());
                }
            }
        }
        let rest: Vec<&&[&str]> = blocks.iter().skip(LOG_KEEP).collect();
        if rest.is_empty() {
            return None;
        }
        out.push(format!("## 提交摘要 · 其余 {} 条（完整提交前 {LOG_KEEP} 条）", rest.len()));
        for (idx, block) in rest.iter().enumerate() {
            if idx >= LOG_SUMMARY_CAP {
                let left = rest.len() - LOG_SUMMARY_CAP;
                out.push(format!("… 其余 {left} 条提交已省略"));
                for b in rest.iter().skip(LOG_SUMMARY_CAP) {
                    omitted += b.iter().filter(|l| !l.is_empty()).count().saturating_sub(1);
                }
                break;
            }
            let sha = block
                .first()
                .and_then(|l| is_commit_header(l))
                .unwrap_or("(unknown)");
            let date = block
                .iter()
                .find(|l| l.starts_with("Date:"))
                .map(|l| {
                    let d = l.trim_start_matches("Date:").trim();
                    let short: String = d.chars().take(24).collect();
                    short.trim().to_string()
                })
                .filter(|d| !d.is_empty());
            let subject = block
                .iter()
                .skip_while(|l| !l.starts_with("Date:"))
                .skip_while(|l| l.starts_with("Date:"))
                .find(|l| !l.is_empty())
                .map(|l| l.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "(无提交说明)".to_string());
            let sha7: String = sha.chars().take(7).collect();
            match date {
                Some(d) => out.push(format!("{sha7}  {d}  {subject}")),
                None => out.push(format!("{sha7}  {subject}")),
            }
            omitted += block.iter().filter(|l| !l.is_empty()).count().saturating_sub(1);
        }
        if omitted < 10 {
            return None;
        }
        return Some(Candidate {
            body: out.join("\n"),
            omitted_lines: omitted,
            hint: "git log".to_string(),
        });
    }

    // 形态 B：git log --oneline。
    let onelines: Vec<(&str, &str)> = lines.iter().filter_map(|l| is_oneline(l)).collect();
    if onelines.len() < 10 {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    let keep = onelines.len().min(12);
    out.push(format!("## git log --oneline 摘要（共 {} 条）", onelines.len()));
    for (sha, subject) in onelines.iter().take(keep) {
        out.push(format!("{sha} {subject}"));
    }
    let rest = onelines.len() - keep;
    out.push(format!(
        "… 其余 {rest} 条已省略（最早一条：{} {}）",
        onelines
            .last()
            .map(|(s, _)| *s)
            .unwrap_or("?"),
        onelines.last().map(|(_, t)| *t).unwrap_or("?")
    ));
    Some(Candidate {
        body: out.join("\n"),
        omitted_lines: rest,
        hint: "git log".to_string(),
    })
}

// ---------------------------------------------------------------------------
// 过滤器 4：grep 结果
// ---------------------------------------------------------------------------

/// 解析 `path:line:content` 或 `path:line-content`（grep -n / -A / -C 的常见形态）。
fn parse_grep_line(line: &str) -> Option<(&str, &str, &str)> {
    let bytes = line.as_bytes();
    for (p, &b) in bytes.iter().enumerate() {
        if b != b':' {
            continue;
        }
        if p == 0 || p > 260 {
            continue;
        }
        let mut q = p + 1;
        let mut digits = 0usize;
        while q < bytes.len() && bytes[q].is_ascii_digit() && digits < 8 {
            q += 1;
            digits += 1;
        }
        if digits == 0 || q >= bytes.len() {
            continue;
        }
        let sep = bytes[q];
        if sep != b':' && sep != b'-' {
            continue;
        }
        let path = &line[..p];
        if path.contains('"') || path.starts_with(' ') {
            continue;
        }
        return Some((path, &line[p + 1..q], &line[q + 1..]));
    }
    None
}

fn grep_result(raw: &str) -> Option<Candidate> {
    let lines: Vec<&str> = raw.split('\n').collect();
    let hits: Vec<(&str, &str, &str)> = lines.iter().filter_map(|l| parse_grep_line(l)).collect();
    if hits.len() < 10 {
        return None;
    }
    // 至少 1/3 的行是命中行，避免把正文里偶然的 `a:1:x` 当 grep 结果。
    let non_empty = lines.iter().filter(|l| !l.trim().is_empty()).count().max(1);
    if hits.len() * 3 < non_empty {
        return None;
    }

    let mut files: Vec<(&str, Vec<(&str, &str)>)> = Vec::new();
    for (path, num, content) in &hits {
        if let Some((_, v)) = files.iter_mut().find(|(p, _)| *p == *path) {
            v.push((num, content));
        } else {
            files.push((path, vec![(num, content)]));
        }
    }
    if files.len() > GREP_FILES_CAP {
        return None;
    }

    let mut out: Vec<String> = Vec::new();
    let mut omitted = 0usize;
    out.push(format!(
        "## grep 摘要（{} 处命中 / {} 个文件）",
        hits.len(),
        files.len()
    ));
    for (path, v) in &files {
        out.push(format!("{path}（{} 处命中）：", v.len()));
        for (num, content) in v.iter().take(GREP_KEEP) {
            let c = content.trim_end();
            let c = if c.chars().count() > 160 {
                let s: String = c.chars().take(160).collect();
                format!("{s} …")
            } else {
                c.to_string()
            };
            out.push(format!("  {num}: {c}"));
        }
        if v.len() > GREP_KEEP {
            let rest = v.len() - GREP_KEEP;
            out.push(format!("  … 该文件另有 {rest} 处命中"));
            omitted += rest;
        }
    }
    if hits.len() > GREP_KEEP && omitted == 0 {
        return None;
    }
    if omitted < 10 {
        return None;
    }
    Some(Candidate {
        body: out.join("\n"),
        omitted_lines: omitted,
        hint: "grep -rn".to_string(),
    })
}

// ---------------------------------------------------------------------------
// 过滤器 5：文件列表（ls / find / tree）
// ---------------------------------------------------------------------------

fn looks_like_path(t: &str) -> bool {
    if t.is_empty() || t.len() > 220 {
        return false;
    }
    // 源文本行一律不算路径：diff 的 +/- 行、代码注释的 `//`、hunk 头都可能含 `/`，
    // 若不排除，22 KB 的真实 `git diff` 会被 file_list 抢走并截断（真实踩到的 bug）。
    if t.starts_with('+') || t.starts_with('-') || t.starts_with('@') || t.starts_with('\\') {
        return false;
    }
    if t.contains("//") || t.contains("/*") || t.contains("*/") {
        return false;
    }
    if t.starts_with("total ")
        || t.starts_with("error")
        || t.starts_with("warning")
        || t.starts_with("diff ")
        || t.starts_with("commit ")
    {
        return false;
    }
    // tree 输出：带树形绘制字符，可能含空格。
    if t.contains("├") || t.contains("└") || t.contains("│") {
        return true;
    }
    if t.contains(' ') || t.contains('\t') {
        return false; // 路径行不含内嵌空白（ls -l 的长格式交给别的过滤器）
    }
    if t.contains('/') || t.contains('\\') {
        return true;
    }
    // ls -1 / 单列文件名：单 token、无空白。
    t.split_whitespace().count() == 1 && !t.chars().next().unwrap().is_ascii_digit()
}

fn dir_of(line: &str) -> Option<String> {
    let t = line.trim_start_matches(|c: char| "├└│─ \t".contains(c));
    let t = t.trim_start_matches("./");
    let (head, sep) = match t.find('/') {
        Some(p) => (&t[..p], true),
        None => (t, false),
    };
    if !sep || head.trim().is_empty() {
        return None;
    }
    Some(head.to_string())
}

fn file_list(raw: &str) -> Option<Candidate> {
    let lines: Vec<&str> = raw.split('\n').collect();
    let non_empty: Vec<&str> = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .copied()
        .collect();
    if non_empty.len() < 60 {
        return None;
    }
    let pathy = non_empty.iter().filter(|l| looks_like_path(l)).count();
    if pathy * 4 < non_empty.len() * 3 {
        return None; // 需要 ≥75% 的行像路径
    }

    // 目录聚合，便于回答「哪个目录文件最多」。
    let mut dirs: Vec<(String, usize)> = Vec::new();
    for l in &non_empty {
        if let Some(d) = dir_of(l) {
            if let Some((_, n)) = dirs.iter_mut().find(|(k, _)| *k == d) {
                *n += 1;
            } else {
                dirs.push((d, 1));
            }
        }
    }
    dirs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let mut out: Vec<String> = Vec::new();
    out.push(format!("## 文件列表摘要（共 {} 项）", non_empty.len()));
    for l in non_empty.iter().take(LIST_KEEP) {
        out.push(l.trim_end().to_string());
    }
    let rest = non_empty.len() - LIST_KEEP.min(non_empty.len());
    if rest == 0 {
        return None;
    }
    out.push(format!("… 其余 {rest} 项已省略"));
    if dirs.len() > 1 {
        let shown: Vec<String> = dirs
            .iter()
            .take(8)
            .map(|(d, n)| format!("{d}/（{n} 项）"))
            .collect();
        out.push(format!("目录分布 Top{}：{}", shown.len(), shown.join("、")));
    }
    if rest < 20 {
        return None;
    }
    Some(Candidate {
        body: out.join("\n"),
        omitted_lines: rest,
        hint: "find / ls".to_string(),
    })
}

// ---------------------------------------------------------------------------
// 过滤器 6：相邻重复行
// ---------------------------------------------------------------------------

fn repeat_lines(raw: &str) -> Option<Candidate> {
    let lines: Vec<&str> = raw.split('\n').collect();
    let mut out: Vec<String> = Vec::new();
    let mut omitted = 0usize;
    let mut i = 0usize;
    while i < lines.len() {
        let cur = lines[i];
        let mut run = 1usize;
        while i + run < lines.len() && lines[i + run] == cur {
            run += 1;
        }
        if run >= REPEAT_MIN_RUN && !cur.trim().is_empty() {
            out.push(cur.trim_end().to_string());
            out.push(format!("[重复 ×{run}]"));
            omitted += run - 2;
            i += run;
        } else {
            out.push(cur.trim_end().to_string());
            i += 1;
        }
    }
    if omitted < 20 {
        return None;
    }
    while out.last().map(|l| l.is_empty()).unwrap_or(false) {
        out.pop();
    }
    Some(Candidate {
        body: out.join("\n"),
        omitted_lines: omitted,
        hint: "日志 / 输出原文".to_string(),
    })
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 真实形态样本的构造器 --------------------------------------------

    fn context_line(n: usize) -> String {
        // 贴近真实 Rust 源码的上下文行，长度在 30~70 字节之间浮动。
        match n % 6 {
            0 => format!("    // 上下文行 {n}：用于验证未变更代码被正确折叠"),
            1 => format!("    let value_{n} = compute(config, state);"),
            2 => format!("    if value_{n} > threshold {{ tracing::debug!(\"ok\"); }}"),
            3 => format!("    store.insert(\"key_{n}\", value_{n});"),
            4 => format!("    // TODO: 后续把这一段抽成独立函数，当前先内联"),
            _ => format!("    handle_event(&mut ctx, Event::Tick({n}));"),
        }
    }

    /// 三个 hunk、中间各 60 行未变更上下文的 unified diff。
    fn sample_diff() -> String {
        let mut s = String::new();
        s.push_str("diff --git a/src/agent/runtime.rs b/src/agent/runtime.rs\n");
        s.push_str("index 3f2a1b9..8c4d5e6 100644\n");
        s.push_str("--- a/src/agent/runtime.rs\n");
        s.push_str("+++ b/src/agent/runtime.rs\n");

        // hunk 1
        s.push_str("@@ -12,66 +12,68 @@ impl AgentRuntime {\n");
        for n in 0..30 {
            s.push_str(&context_line(n));
            s.push('\n');
        }
        s.push_str("-    let timeout = Duration::from_secs(120);\n");
        s.push_str("+    let timeout = Duration::from_secs(300);\n");
        s.push_str("+    tracing::info!(\"审批超时调整为 300s\");\n");
        for n in 30..60 {
            s.push_str(&context_line(n));
            s.push('\n');
        }

        // hunk 2
        s.push_str("@@ -120,64 +122,65 @@ fn approval_loop(&mut self) {\n");
        for n in 60..90 {
            s.push_str(&context_line(n));
            s.push('\n');
        }
        s.push_str("-    match self.classifier.classify(cmd) {\n");
        s.push_str("-        Risk::ReadOnly => self.execute(cmd).await,\n");
        s.push_str("+    match self.classifier.classify(cmd) {\n");
        s.push_str("+        Risk::ReadOnly => self.execute(cmd).await,\n");
        s.push_str("+        Risk::Mutating => self.request_approval(cmd).await,\n");
        for n in 90..120 {
            s.push_str(&context_line(n));
            s.push('\n');
        }

        // hunk 3
        s.push_str("@@ -210,58 +213,57 @@ fn build_prompt(&self) -> String {\n");
        for n in 120..150 {
            s.push_str(&context_line(n));
            s.push('\n');
        }
        s.push_str("-    let mut prompt = String::with_capacity(4096);\n");
        s.push_str("-    prompt.push_str(&self.system_prompt);\n");
        s.push_str("+    let prompt = format!(\"{}\\n{}\", self.system_prompt, self.tools_desc);\n");
        for n in 150..180 {
            s.push_str(&context_line(n));
            s.push('\n');
        }
        s
    }

    /// 真实 `git status` 长格式，三个分区各 20 个文件（合计 60 个）。
    fn sample_git_status() -> String {
        let mut s = String::new();
        s.push_str("On branch feat/model-providers\n");
        s.push_str("Your branch is up to date with 'origin/main'.\n\n");
        s.push_str("Changes to be committed:\n");
        s.push_str("  (use \"git restore --staged <file>...\" to unstage)\n");
        for n in 0..20 {
            let tag = match n % 3 {
                0 => "new file",
                1 => "modified",
                _ => "renamed",
            };
            s.push_str(&format!(
                "\t{tag}:   src-tauri/src/llm/channel_{n}.rs\n"
            ));
        }
        s.push_str("\nChanges not staged for commit:\n");
        s.push_str("  (use \"git add <file>...\" to update what will be committed)\n");
        for n in 0..20 {
            s.push_str(&format!(
                "\tmodified:   src/features/settings/section_{n}.tsx\n"
            ));
        }
        s.push_str("\nUntracked files:\n");
        s.push_str("  (use \"git add <file>...\" to include in what will be committed)\n");
        for n in 0..20 {
            s.push_str(&format!("\tdocs/notes/plan-{n}.md\n"));
        }
        s.push_str("\nno changes added to commit (use \"git add\" and \"git commit -a\")\n");
        s
    }

    /// 十二个 commit 的完整 `git log --stat`（真实 `git log -20` 的典型规模）。
    fn sample_git_log() -> String {
        let mut s = String::new();
        for n in 0..12 {
            let sha = format!("{:040x}", 0xa1b2c3d4u64 + n as u64);
            s.push_str(&format!("commit {sha}\n"));
            s.push_str(&format!(
                "Author: dev-{n} <dev-{n}@modern-sshdeck.local>\n"
            ));
            s.push_str(&format!("Date:   Sun Nov 1 09:{:02}:00 2026 +0800\n\n", n));
            s.push_str(&format!("    feat(llm): 接入第 {n} 个聚合渠道\n\n"));
            for k in 0..5 {
                s.push_str(&format!(
                    " src-tauri/src/llm/registry_{n}_{k}.rs | {:>3} +++++--\n",
                    10 + k * 7
                ));
            }
            s.push_str(" 5 files changed, 88 insertions(+), 21 deletions(-)\n\n");
        }
        s
    }

    /// 200 行的 `find src -name '*.rs'` 输出。
    fn sample_find() -> String {
        let mut s = String::new();
        let dirs = [
            "src-tauri/src/agent",
            "src-tauri/src/guard",
            "src-tauri/src/llm",
            "src-tauri/src/ipc",
            "src-tauri/src/ssh",
            "src-tauri/src/persist",
            "src/features/settings",
            "src/features/terminal",
            "src/shared",
            "scripts",
        ];
        for n in 0..200 {
            let d = dirs[n % dirs.len()];
            s.push_str(&format!("{d}/module_{n}.rs\n"));
        }
        s
    }

    /// 同一文件 30 行命中 + 另外两个文件少量命中的 grep 输出。
    fn sample_grep() -> String {
        let mut s = String::new();
        for n in 0..30 {
            s.push_str(&format!(
                "src-tauri/src/token/rtk.rs:{n}:    fn filter_{n}(raw: &str) -> Option<String> {{\n"
            ));
        }
        for n in 0..8 {
            s.push_str(&format!(
                "src-tauri/src/agent/runtime.rs:{}:    compress_tool_output(&out)\n",
                100 + n
            ));
        }
        for n in 0..5 {
            s.push_str(&format!(
                "src/features/settings/TokenSection.tsx:{}:    savedPercent\n",
                20 + n
            ));
        }
        s
    }

    /// 相邻重复的构建日志：三种行各自重复 80/40/60 次。
    fn sample_repeat() -> String {
        let mut s = String::new();
        for n in 0..80 {
            s.push_str(&format!(
                "[{:02}:{:02}:{:02}] downloading crate #{n}\n",
                n % 60,
                n % 60,
                n % 60
            ));
        }
        // 用一条真正重复的行触发折叠
        s.clear();
        for _ in 0..80 {
            s.push_str("Compiling proc-macro2 v1.0.86\n");
        }
        for _ in 0..40 {
            s.push_str("warning: unused variable: `ctx`\n");
        }
        for _ in 0..60 {
            s.push_str("   Compiling serde v1.0.210\n");
        }
        s.push_str("    Finished `dev` profile [unoptimized] target(s) in 12.4s\n");
        s
    }

    fn all_samples() -> Vec<(&'static str, String)> {
        vec![
            ("diff", sample_diff()),
            ("status", sample_git_status()),
            ("log", sample_git_log()),
            ("find", sample_find()),
            ("grep", sample_grep()),
            ("repeat", sample_repeat()),
        ]
    }

    fn ratio(outcome: &CompressOutcome) -> f64 {
        outcome.saved_bytes() as f64 / outcome.before_bytes as f64
    }

    // ---- 6 个过滤器各 1 例 ------------------------------------------------

    #[test]
    fn unified_diff_folds_unchanged_context() {
        let raw = sample_diff();
        assert!(raw.len() > 8000, "样本要足够大，实际 {}", raw.len());
        let out = compress_tool_output(&raw);
        assert_eq!(out.filter, Some("unified_diff"), "应命中 diff 过滤器");
        assert!(out.applied);
        assert!(
            ratio(&out) > 0.15,
            "diff 压缩率过低：{:.1}%（{} → {}）",
            ratio(&out) * 100.0,
            out.before_bytes,
            out.after_bytes
        );

        // 安全性：所有 +/- 变更行、文件头、hunk 头必须原样保留。
        for line in raw.lines() {
            if is_change_line(line) || is_hunk_header(line) || line.starts_with("diff --git ") {
                assert!(
                    out.text.contains(line.trim_end()),
                    "变更行丢失，压缩不可接受：{line}"
                );
            }
        }
        assert!(out.text.contains("省略"), "必须标注省略行数");
    }

    #[test]
    fn git_status_reports_three_categories() {
        let raw = sample_git_status();
        let out = compress_tool_output(&raw);
        assert_eq!(out.filter, Some("git_status"), "应命中 status 过滤器");
        assert!(out.applied);
        // 三类状态都要出现，且带计数 —— 模型据此仍能答「多少文件待提交」。
        for label in ["已暂存", "未暂存修改", "未跟踪"] {
            assert!(out.text.contains(label), "摘要缺少分类 {label}");
        }
        assert!(out.text.contains("共 60 个文件"), "缺少文件总数：\n{}", out.text);
        assert!(ratio(&out) > 0.3, "status 压缩率过低：{:.1}%", ratio(&out) * 100.0);
    }

    #[test]
    fn git_log_keeps_recent_and_summarizes_rest() {
        let raw = sample_git_log();
        let out = compress_tool_output(&raw);
        assert_eq!(out.filter, Some("git_log"), "应命中 log 过滤器");
        assert!(out.applied);
        // 第一条提交必须完整保留（sha + 作者 + 日期 + 说明）。
        assert!(out.text.contains("Author: dev-0"), "首条提交信息丢失");
        assert!(out.text.contains("Date:   Sun Nov 1"), "首条提交日期丢失");
        assert!(out.text.contains("接入第 0 个聚合渠道"), "首条提交说明丢失");
        assert!(out.text.contains("提交摘要"), "缺少其余提交摘要");
        assert!(ratio(&out) > 0.4, "log 压缩率过低：{:.1}%", ratio(&out) * 100.0);
    }

    #[test]
    fn file_list_keeps_head_and_reports_rest() {
        let raw = sample_find();
        assert!(raw.lines().count() == 200);
        let out = compress_tool_output(&raw);
        assert_eq!(out.filter, Some("file_list"), "应命中文件列表过滤器");
        assert!(out.applied);
        assert!(out.text.contains("共 200 项"), "缺少总数");
        assert!(out.text.contains("其余 160 项已省略"), "缺少余项说明：\n{}", out.text);
        assert!(out.text.contains("目录分布"), "缺少目录聚合");
        assert!(ratio(&out) > 0.5, "列表压缩率过低：{:.1}%", ratio(&out) * 100.0);
    }

    #[test]
    fn grep_result_aggregates_by_file() {
        let raw = sample_grep();
        let out = compress_tool_output(&raw);
        assert_eq!(out.filter, Some("grep_result"), "应命中 grep 过滤器");
        assert!(out.applied);
        assert!(out.text.contains("43 处命中 / 3 个文件"), "命中统计不对：\n{}", out.text);
        assert!(out.text.contains("src-tauri/src/token/rtk.rs（30 处命中）"));
        assert!(out.text.contains("该文件另有 24 处命中"));
        assert!(ratio(&out) > 0.5, "grep 压缩率过低：{:.1}%", ratio(&out) * 100.0);
    }

    #[test]
    fn repeat_lines_collapses_adjacent_runs() {
        let raw = sample_repeat();
        let out = compress_tool_output(&raw);
        assert_eq!(out.filter, Some("repeat_lines"), "应命中重复行过滤器");
        assert!(out.applied);
        assert!(out.text.contains("[重复 ×80]"), "80 次重复未折叠：\n{}", out.text);
        assert!(out.text.contains("[重复 ×40]"));
        assert!(out.text.contains("[重复 ×60]"));
        assert!(out.text.contains("Finished `dev` profile"), "尾部结果行丢失");
        assert!(ratio(&out) > 0.5, "重复行压缩率过低：{:.1}%", ratio(&out) * 100.0);
    }

    // ---- §4.2 安全规则 ----------------------------------------------------

    #[test]
    fn safety_error_output_is_never_compressed() {
        // 首行报错 + 大量重复行：本可被 repeat_lines 压掉 70%，但必须放行。
        let raw = format!("error: failed to connect to 10.0.0.12:22\n{}", sample_repeat());
        let out = compress_tool_output(&raw);
        assert!(!out.applied, "报错输出被压缩了，违反 §4.2 规则 1");
        assert_eq!(out.filter, None);
        assert_eq!(out.text, raw, "报错输出必须原样放行");

        // panic / Traceback 同理。
        let raw2 = format!("Traceback (most recent call last):\n{}", sample_find());
        assert!(!compress_tool_output(&raw2).applied);

        // 良性大 diff 里恰好出现 error 字样，不应被误判。
        let mut benign = sample_diff();
        benign.push_str("+    eprintln!(\"error: unexpected state\");\n");
        assert!(
            compress_tool_output(&benign).applied,
            "正文含 error 字样的良性 diff 被误判为报错"
        );
    }

    #[test]
    fn safety_small_output_is_not_compressed() {
        let raw = "Compiling serde v1.0.210\n".repeat(60); // ~1.5 KB
        assert!(raw.len() < MIN_BYTES);
        let out = compress_tool_output(&raw);
        assert!(!out.applied, "小于 MIN_BYTES 的输出被压缩了，违反 §4.2 规则 2");
        assert_eq!(out.text, raw);
        assert_eq!(out.after_bytes, out.before_bytes);
    }

    #[test]
    fn safety_result_never_larger_than_input() {
        for (name, raw) in all_samples() {
            let out = compress_tool_output(&raw);
            assert!(
                out.after_bytes < out.before_bytes,
                "{name}: 压缩结果比原文大（{} ≥ {}），违反 §4.2 规则 3",
                out.after_bytes,
                out.before_bytes
            );
            assert_eq!(out.before_bytes, raw.len() as u64, "{name}: before_bytes 失真");
            assert_eq!(out.after_bytes, out.text.len() as u64, "{name}: after_bytes 失真");
        }

        // 明确构造「过滤器想动手但收益为负」的情形：每行极短的 git log，
        // 折叠后摘要头反而更长 → 必须回退原文。
        let mut tiny_log = String::new();
        for n in 0..8 {
            tiny_log.push_str(&format!("commit {:07x}\n", 0xab0 + n));
        }
        let out = compress_tool_output(&tiny_log);
        assert!(!out.applied, "负收益时必须回退原文，违反 §4.2 规则 3");
        assert_eq!(out.text, tiny_log);
    }

    #[test]
    fn safety_compression_is_idempotent() {
        for (name, raw) in all_samples() {
            let first = compress_tool_output(&raw);
            assert!(first.applied, "{name}: 样本应被压缩");
            let second = compress_tool_output(&first.text);
            assert!(
                !second.applied,
                "{name}: 二次压缩又动手了，违反 §4.2 规则 4（幂等）"
            );
            assert_eq!(second.text, first.text, "{name}: 二次压缩结果与首次不同");
            assert_eq!(second.before_bytes, first.after_bytes);
        }
    }

    #[test]
    fn safety_plain_text_passes_through_untouched() {
        // 多行散文，每行不同、含标点与括号，不命中任何过滤器 → 逐字节原样放行。
        let mut raw = String::new();
        for n in 0..140 {
            raw.push_str(&format!(
                "第 {n} 段说明：这段文字用于确认未命中过滤器时输出完全不变，包含标点（），以及中文逗号、感叹号！\n"
            ));
        }
        let out = compress_tool_output(&raw);
        assert!(!out.applied, "散文被误判压缩了：filter={:?}", out.filter);
        assert_eq!(out.text, raw, "未命中过滤器时输出必须逐字节不变");
        assert_eq!(out.filter, None);
    }

    // ---- 辅助函数的边界 ---------------------------------------------------

    #[test]
    fn helpers_classify_status_codes() {
        assert_eq!(short_code_label('A', ' '), Some("已暂存"));
        assert_eq!(short_code_label('M', ' '), Some("已暂存"));
        assert_eq!(short_code_label(' ', 'M'), Some("未暂存修改"));
        assert_eq!(short_code_label('?', '?'), Some("未跟踪"));
        assert_eq!(short_code_label('U', 'U'), Some("冲突未解决"));
        assert_eq!(short_code_label('M', 'M'), Some("已暂存且未暂存"));
        assert_eq!(short_code_label(' ', ' '), None);
        assert_eq!(short_code_label('x', 'y'), None, "非法状态码不应归类");
    }

    #[test]
    fn helpers_parse_grep_lines() {
        assert_eq!(
            parse_grep_line("src/main.rs:12:fn main()"),
            Some(("src/main.rs", "12", "fn main()"))
        );
        // grep -C 的上下文行分隔符是 '-' 而不是 ':'
        assert_eq!(
            parse_grep_line("src/main.rs:13-    let x = 1;"),
            Some(("src/main.rs", "13", "    let x = 1;"))
        );
        assert_eq!(parse_grep_line("没有行号的一行说明"), None);
        assert_eq!(parse_grep_line("http://example.com"), None, "URL 不该被当成命中");
    }

    #[test]
    fn helpers_marker_and_byte_formatting() {
        assert_eq!(human_bytes(900), "900 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(41_164), "40.2 KB");
        assert_eq!(human_bytes(3_993), "3.9 KB");
        assert!(build_marker(41_164, 3_993, 214, "git diff").starts_with(MARKER_PREFIX));
        assert!(is_compressed(&build_marker(100, 20, 3, "ls")));
        assert!(!is_compressed("diff --git a/x b/x"));
    }

    #[test]
    fn unified_diff_keeps_original_ordering_and_caps_huge_diffs() {
        // 回归测试：多个文件的 diff，每个文件的头必须紧跟自己的 hunk，
        // 不能像早期实现那样把所有 `diff --git` 头堆到最前面（真实踩到的 bug）。
        let mut raw = String::new();
        for f in 0..4 {
            raw.push_str(&format!("diff --git a/src/f{f}.rs b/src/f{f}.rs\n"));
            raw.push_str(&format!("index 1111111..2222222 100644\n"));
            raw.push_str(&format!("--- a/src/f{f}.rs\n"));
            raw.push_str(&format!("+++ b/src/f{f}.rs\n"));
            raw.push_str("@@ -1,60 +1,60 @@\n");
            // 60 行密集变更，中间没有一段可折叠的纯上下文 —— 只有硬上限能救。
            for n in 0..60 {
                raw.push_str(&format!("-    // old line {n} in file {f}\n"));
                raw.push_str(&format!("+    // new line {n} in file {f}\n"));
            }
        }
        assert!(raw.len() > 8000, "样本要够大，实际 {}", raw.len());
        let out = compress_tool_output(&raw);
        assert_eq!(out.filter, Some("unified_diff"), "应命中 diff 过滤器");
        assert!(out.applied);
        assert!(
            ratio(&out) > 0.5,
            "无上下文的巨型 diff 压缩率过低：{:.1}%",
            ratio(&out) * 100.0
        );

        // 顺序：每个 `diff --git` 块后面的第一条 `@@` 必须是同一文件的 hunk。
        let txt = &out.text;
        for f in 0..4 {
            let file_pos = txt
                .find(&format!("diff --git a/src/f{f}.rs"))
                .unwrap_or_else(|| panic!("文件 f{f} 的头丢失"));
            let hunk_pos = txt[file_pos..]
                .find("@@ -1,60 +1,60 @@")
                .unwrap_or_else(|| panic!("文件 f{f} 的 hunk 头丢失"));
            // 该 hunk 之后到下一个文件头之前，不应出现别的文件的 hunk。
            assert!(
                hunk_pos < 200,
                "文件 f{f} 的头与 hunk 之间隔了 {hunk_pos} 字节，说明顺序被打乱"
            );
        }
        // 硬上限必须生效，并如实告知抽稀了多少变更行。
        assert!(
            txt.contains("因超长被抽稀省略"),
            "超长 diff 必须标注抽稀，实际：\n{}",
            txt
        );
    }
}
