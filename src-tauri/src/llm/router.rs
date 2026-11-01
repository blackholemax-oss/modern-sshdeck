//! 路由与降级链（M-A3）。
//!
//! 契约见 `docs/FEATURE-MODEL-PROVIDERS.md` §3.2 与 §3.3。
//!
//! 本模块**只做纯逻辑**：解析 `provider/model`、合并渠道配置得出真实目标、
//! 按 fallback 顺序推进降级链。真正的网络调用由调用方通过 [`ChatRunner`] 注入，
//! 因此这里不依赖 `llm::openai`，也不触碰 `state.rs` / `ipc`。
//!
//! 明确不做（契约 §1.2）：`round-robin` 粘滞策略。策略只有 `fallback`。

use std::collections::HashMap;
use std::fmt;

use crate::error::{AppError, AppResult};
use crate::llm::openai::normalize_base_url;
use crate::llm::registry::{self, AuthKind, ProviderCategory, ProviderEntry, BUILTIN_ID};

/// 一个渠道的用户侧配置（对应契约 §3.3 的 `ChannelPatch` 解析结果）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelConfig {
    pub provider_id: String,
    pub enabled: bool,
    /// 用户覆盖的端点。
    pub base_url_override: Option<String>,
    /// 用户填的 Key（内存态，不落盘到此处）。
    pub api_key: Option<String>,
    /// 覆盖模型清单。
    pub model_overrides: Option<Vec<String>>,
}

impl From<crate::ipc::dto::ChannelConfig> for ChannelConfig {
    fn from(c: crate::ipc::dto::ChannelConfig) -> Self {
        Self {
            provider_id: c.provider_id,
            enabled: c.enabled,
            base_url_override: c.base_url_override,
            api_key: c.api_key,
            model_overrides: c.model_overrides,
        }
    }
}

impl ChannelConfig {
    pub fn new(provider_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            enabled: true,
            base_url_override: None,
            api_key: None,
            model_overrides: None,
        }
    }
}

/// 用户直接填 base_url + key 的老路径（契约 §3.2 的 `builtin/`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefaultLlmConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
}

/// 解析后的调用目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    /// 命中的注册表项；`builtin` 路径会构造一个合成的 `builtin` 条目。
    pub provider: ProviderEntry,
    /// 真正的模型 id（已剥掉 `provider/` 前缀）。
    pub model: String,
    /// 归一化后的 base_url。
    pub base_url: String,
    /// 运行时 Key；本机/免 Key 通道为 `None`。
    pub api_key: Option<String>,
}

impl ResolvedTarget {
    /// 回写为统一的 `provider/model` 标识。
    pub fn qualified(&self) -> String {
        self.provider.qualify(&self.model)
    }
}

/// `builtin` 伪提供商：合成条目，不出现在注册表选择器里。
fn builtin_entry() -> ProviderEntry {
    ProviderEntry {
        id: BUILTIN_ID,
        name: "自定义端点",
        category: ProviderCategory::ApiKey,
        base_url: None,
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("用户直接填写 base_url + Key 的传统路径"),
    }
}

// ─────────────────────────── 解析 ───────────────────────────

/// 解析统一模型标识 `provider/model`（契约 §3.2）。
///
/// 规则：
/// - **首个** `/` 之前是 `provider_id`，其余整体是 `model_id`
///   （`model_id` 自身可含 `/`，如 `openrouter/anthropic/claude-3.5`）。
/// - `provider_id` 不在注册表内 → 整串视为 `builtin/` 下的模型。
/// - 没有 `/` → 整串也是 `builtin/` 下的模型。
/// - 空串 → `("", "")`，由调用方报错。
pub fn parse_model_id(s: &str) -> (&str, &str) {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return (BUILTIN_ID, "");
    }

    match trimmed.split_once('/') {
        Some((head, rest)) => {
            let head = head.trim();
            // `builtin/xxx` 显式写法
            if head == BUILTIN_ID {
                return (BUILTIN_ID, rest.trim());
            }
            if registry::find(head).is_some() {
                (head, rest.trim())
            } else {
                // 未知 provider：整串当 builtin 模型（保持原样，不丢信息）
                (BUILTIN_ID, trimmed)
            }
        }
        // 无斜杠：老路径模型名
        None => (BUILTIN_ID, trimmed),
    }
}

// ─────────────────────────── 解析目标 ───────────────────────────

/// 把 `active_model` 解析成可调用的目标。
///
/// - `channels` 提供每个 provider 的 Key / 端点覆盖（按 `provider_id` 索引）。
/// - `default_llm_config` 是 `builtin/` 路径的兜底配置。
pub fn resolve(
    active_model: &str,
    channels: &HashMap<String, ChannelConfig>,
    default_llm_config: &DefaultLlmConfig,
) -> AppResult<ResolvedTarget> {
    let (provider_id, model_id) = parse_model_id(active_model);

    if model_id.is_empty() {
        return Err(AppError::Llm(format!(
            "模型标识为空（收到 {active_model:?}）；请填 `provider/model` 形式，如 `ollama/qwen2.5:7b`"
        )));
    }

    if provider_id == BUILTIN_ID {
        return resolve_builtin(model_id, channels, default_llm_config);
    }

    let entry = registry::find(provider_id)
        .ok_or_else(|| AppError::Llm(format!("未知提供商 {provider_id}")))?;

    // 注：这里曾拒绝「需交互式 OAuth」的通道。该类条目（Gemini / Google ADC）
    // 已从注册表移除，`is_drivable()` 恒为 true，故该分支成为死代码并删除。
    let _ = &entry;

    let channel = channels.get(provider_id);

    // 渠道被显式禁用
    if let Some(c) = channel {
        if !c.enabled {
            return Err(AppError::Llm(format!(
                "渠道 {} 已被禁用，请先在设置里启用",
                entry.name
            )));
        }
    }

    // 端点：渠道覆盖 > 注册表默认
    let base_url = match channel.and_then(|c| c.base_url_override.clone()) {
        Some(v) if !v.trim().is_empty() => normalize_base_url(&v),
        _ => match entry.base_url {
            Some(u) => normalize_base_url(u),
            None => {
                return Err(AppError::Llm(format!(
                    "{} 没有默认端点，需在设置里手工填写 base_url",
                    entry.name
                )));
            }
        },
    };

    // Key：注册表声明无需认证时不传（本机通道即使填了 Key 也忽略）
    let api_key = match entry.auth {
        AuthKind::None => None,
        AuthKind::Bearer => channel
            .and_then(|c| c.api_key.clone())
            .filter(|k| !k.trim().is_empty()),
    };

    // 需要 Bearer 但没有 Key → 尽早报错，而不是发出请求后被 401
    if entry.auth == AuthKind::Bearer && api_key.is_none() {
        return Err(AppError::Llm(format!(
            "{} 需要 API Key，尚未配置",
            entry.name
        )));
    }

    // 模型覆盖校验：用户自定义了清单时，模型必须落在清单内
    if let Some(list) = channel.and_then(|c| c.model_overrides.clone()) {
        if !list.is_empty() && !list.iter().any(|m| m.trim() == model_id) {
            return Err(AppError::Llm(format!(
                "{} 的模型清单里没有 {}，请更新渠道的模型覆盖列表",
                entry.name, model_id
            )));
        }
    }

    Ok(ResolvedTarget {
        provider: *entry,
        model: model_id.to_string(),
        base_url,
        api_key,
    })
}

/// `builtin/` 路径：用用户自填的 base_url / key。
fn resolve_builtin(
    model_id: &str,
    channels: &HashMap<String, ChannelConfig>,
    default_llm_config: &DefaultLlmConfig,
) -> AppResult<ResolvedTarget> {
    let channel = channels.get(BUILTIN_ID);

    let raw_base = match channel.and_then(|c| c.base_url_override.clone()) {
        Some(v) if !v.trim().is_empty() => v,
        _ => default_llm_config.base_url.clone(),
    };
    if raw_base.trim().is_empty() {
        return Err(AppError::Llm(
            "自定义端点未配置 base_url；请在设置里填写，或改用 `provider/model` 形式".into(),
        ));
    }

    let api_key = match channel.and_then(|c| c.api_key.clone()).filter(|k| !k.trim().is_empty()) {
        Some(k) => Some(k),
        None => Some(default_llm_config.api_key.clone()).filter(|k| !k.trim().is_empty()),
    };

    Ok(ResolvedTarget {
        provider: builtin_entry(),
        model: model_id.to_string(),
        base_url: normalize_base_url(&raw_base),
        api_key,
    })
}

// ─────────────────────────── 降级链 ───────────────────────────

/// 降级策略。契约 §1.2 明确不做 `round-robin`，故只有 `Fallback`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ComboStrategy {
    /// 按顺序尝试，第一个成功即返回。
    #[default]
    Fallback,
}

impl ComboStrategy {
    pub fn parse(s: &str) -> AppResult<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "fallback" => Ok(ComboStrategy::Fallback),
            other => Err(AppError::Llm(format!(
                "不支持的降级策略 {other:?}；本应用只支持 \"fallback\""
            ))),
        }
    }
}

/// 降级链（对应契约 §3.3 的 `ComboPatch`），以 `combo:<name>` 暴露。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Combo {
    pub name: String,
    pub strategy: ComboStrategy,
    /// 成员，按尝试顺序；元素形如 `provider/model`。
    pub members: Vec<String>,
}

impl Combo {
    pub fn new(name: impl Into<String>, members: Vec<String>) -> Self {
        Self {
            name: name.into(),
            strategy: ComboStrategy::Fallback,
            members,
        }
    }

    /// 对外的模型标识：`combo:<name>`，可直接作为 `active_model`。
    pub fn qualified(&self) -> String {
        format!("combo:{}", self.name)
    }

    /// 是否被 `active_model` 指向。
    pub fn matches(&self, active_model: &str) -> bool {
        let a = active_model.trim();
        a.strip_prefix("combo:")
            .map(|n| n.trim() == self.name)
            .unwrap_or(false)
    }
}

/// `combo:<name>` 标识的解析结果。
pub fn parse_combo_id(s: &str) -> Option<&str> {
    s.trim().strip_prefix("combo:").map(str::trim).filter(|n| !n.is_empty())
}

/// 单次尝试的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptOutcome {
    /// 实际被尝试的 `provider/model`。
    pub target: String,
    /// 该次尝试拿到的回复文本（成功时）。
    pub output: Option<String>,
    /// 失败原因（失败时）。
    pub error: Option<String>,
}

impl AttemptOutcome {
    pub fn success(target: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            output: Some(output.into()),
            error: None,
        }
    }

    pub fn failure(target: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            output: None,
            error: Some(error.into()),
        }
    }

    pub fn is_success(&self) -> bool {
        self.output.is_some()
    }
}

/// 降级链整体结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainReport {
    /// 依次尝试的完整轨迹（含失败），供 UI 显示「已降级到第 n 个」。
    pub attempts: Vec<AttemptOutcome>,
    /// 最终成功的那次尝试的下标。
    pub winner: Option<usize>,
}

impl ChainReport {
    /// 最终的回复文本（全链失败时为 `None`）。
    pub fn output(&self) -> Option<&str> {
        self.winner
            .and_then(|i| self.attempts.get(i))
            .and_then(|a| a.output.as_deref())
    }

    /// 是否发生过降级（即成功的不是第一个）。
    pub fn degraded(&self) -> bool {
        matches!(self.winner, Some(i) if i > 0)
    }

    /// 全链失败时的汇总错误。
    pub fn to_error(&self) -> AppError {
        let detail = self
            .attempts
            .iter()
            .map(|a| {
                format!(
                    "{}: {}",
                    a.target,
                    a.error.as_deref().unwrap_or("未知原因")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        AppError::Llm(format!(
            "降级链全部失败（共 {} 个候选）：{detail}",
            self.attempts.len()
        ))
    }
}

/// 单次调用的抽象。调用方注入真实实现（通常是 `OpenAiCompatProvider`），
/// 本模块因此不与 `openai.rs` 耦合。
#[async_trait::async_trait]
pub trait ChatRunner: Send + Sync {
    /// 对解析后的目标发起一次对话，成功返回回复文本。
    async fn run(&self, target: &ResolvedTarget) -> AppResult<String>;
}

/// 记录每次尝试的降级链执行器（纯逻辑部分）。
///
/// 语义：**严格按 `combo.members` 顺序**尝试；第一个成功即返回，不再尝试后续成员。
/// 单个成员解析失败（未知 provider / 缺 Key）也计为一次失败尝试并继续降级 ——
/// 这正是降级链的价值：前一个渠道配错了，后面的还能顶上。
pub async fn run_combo_chain(
    combo: &Combo,
    channels: &HashMap<String, ChannelConfig>,
    default_llm_config: &DefaultLlmConfig,
    runner: &dyn ChatRunner,
) -> ChainReport {
    let mut attempts: Vec<AttemptOutcome> = Vec::new();
    let mut winner: Option<usize> = None;

    for member in &combo.members {
        if winner.is_some() {
            break;
        }

        let idx = attempts.len();
        match resolve(member, channels, default_llm_config) {
            Ok(target) => match runner.run(&target).await {
                Ok(text) => {
                    attempts.push(AttemptOutcome::success(member.clone(), text));
                    winner = Some(idx);
                }
                Err(e) => attempts.push(AttemptOutcome::failure(member.clone(), e.message())),
            },
            Err(e) => attempts.push(AttemptOutcome::failure(member.clone(), e.message())),
        }
    }

    ChainReport { attempts, winner }
}

/// 给定 `active_model`，若它指向某个 combo 则返回该 combo。
pub fn find_combo<'a>(active_model: &str, combos: &'a [Combo]) -> Option<&'a Combo> {
    parse_combo_id(active_model).and_then(|name| combos.iter().find(|c| c.name == name))
}

/// 解析 `active_model`：命中 combo 则解析其首个成员作为「预览目标」，
/// 否则按普通 `provider/model` 解析。供 UI 展示「当前实际会用哪个端点」。
pub fn resolve_preview(
    active_model: &str,
    channels: &HashMap<String, ChannelConfig>,
    combos: &[Combo],
    default_llm_config: &DefaultLlmConfig,
) -> AppResult<ResolvedTarget> {
    if let Some(combo) = find_combo(active_model, combos) {
        let first = combo.members.first().ok_or_else(|| {
            AppError::Llm(format!("降级链 {} 没有任何成员", combo.name))
        })?;
        return resolve(first, channels, default_llm_config);
    }
    resolve(active_model, channels, default_llm_config)
}

// 让 `ResolvedTarget` 便于日志输出（不含 Key）
impl fmt::Display for ResolvedTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{} @ {}", self.provider.id, self.model, self.base_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::registry::find;
    use std::sync::Mutex;

    fn channels_with(entries: &[(&str, &str)]) -> HashMap<String, ChannelConfig> {
        let mut m = HashMap::new();
        for (id, key) in entries {
            let mut c = ChannelConfig::new(*id);
            c.api_key = Some((*key).to_string());
            m.insert(c.provider_id.clone(), c);
        }
        m
    }

    fn builtin_cfg() -> DefaultLlmConfig {
        DefaultLlmConfig {
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-chat".into(),
            api_key: "sk-builtin".into(),
        }
    }

    // ───────────── parse_model_id ─────────────

    #[test]
    fn parses_known_provider() {
        assert_eq!(parse_model_id("ollama/qwen2.5:7b"), ("ollama", "qwen2.5:7b"));
        assert_eq!(parse_model_id("openrouter/anthropic/claude-3.5"), ("openrouter", "anthropic/claude-3.5"));
        assert_eq!(parse_model_id("kilo/kilo-auto-free"), ("kilo", "kilo-auto-free"));
    }

    #[test]
    fn splits_on_first_slash_only() {
        // model_id 自身含 `/` 时必须完整保留
        let (p, m) = parse_model_id("openrouter/meta-llama/llama-3.1-70b-instruct");
        assert_eq!(p, "openrouter");
        assert_eq!(m, "meta-llama/llama-3.1-70b-instruct");
    }

    #[test]
    fn unknown_provider_falls_back_to_builtin() {
        // 契约 §3.2：未知 provider_id → 整个字符串视为 builtin 下的模型
        let (p, m) = parse_model_id("my-private-gateway/gpt-4");
        assert_eq!(p, BUILTIN_ID);
        assert_eq!(m, "my-private-gateway/gpt-4");

        let (p, m) = parse_model_id("no-slash-model");
        assert_eq!(p, BUILTIN_ID);
        assert_eq!(m, "no-slash-model");
    }

    #[test]
    fn explicit_builtin_prefix_is_stripped() {
        let (p, m) = parse_model_id("builtin/deepseek-chat");
        assert_eq!(p, BUILTIN_ID);
        assert_eq!(m, "deepseek-chat");
    }

    #[test]
    fn empty_and_whitespace_inputs() {
        assert_eq!(parse_model_id(""), (BUILTIN_ID, ""));
        assert_eq!(parse_model_id("   "), (BUILTIN_ID, ""));
        assert_eq!(parse_model_id(" ollama / qwen2.5:7b "), ("ollama", "qwen2.5:7b"));
    }

    // ───────────── resolve ─────────────

    #[test]
    fn resolves_local_without_key() {
        let t = resolve("ollama/qwen2.5:7b", &HashMap::new(), &builtin_cfg()).unwrap();
        assert_eq!(t.provider.id, "ollama");
        assert_eq!(t.model, "qwen2.5:7b");
        assert_eq!(t.base_url, "http://127.0.0.1:11434/v1");
        assert!(t.api_key.is_none(), "本机通道不应带 Key");
        assert_eq!(t.qualified(), "ollama/qwen2.5:7b");
    }

    #[test]
    fn local_channel_ignores_user_supplied_key() {
        let ch = channels_with(&[("ollama", "sk-should-be-ignored")]);
        let t = resolve("ollama/qwen2.5:7b", &ch, &builtin_cfg()).unwrap();
        assert!(t.api_key.is_none(), "auth=None 的通道即便填了 Key 也必须忽略");
    }

    #[test]
    fn resolves_bearer_provider_with_key() {
        let ch = channels_with(&[("openrouter", "sk-or-xxx")]);
        let t = resolve("openrouter/anthropic/claude-3.5", &ch, &builtin_cfg()).unwrap();
        assert_eq!(t.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(t.api_key.as_deref(), Some("sk-or-xxx"));
        assert_eq!(t.model, "anthropic/claude-3.5");
    }

    #[test]
    fn missing_key_for_bearer_provider_errors_early() {
        let err = resolve("openrouter/anthropic/claude-3.5", &HashMap::new(), &builtin_cfg())
            .unwrap_err();
        assert!(err.message().contains("需要 API Key"));
    }

    #[test]
    fn base_url_override_wins_and_is_normalized() {
        let mut ch = HashMap::new();
        let mut c = ChannelConfig::new("openrouter");
        c.api_key = Some("sk".into());
        c.base_url_override = Some("https://my-proxy.local".into()); // 无 /v1、无尾斜杠
        ch.insert("openrouter".into(), c);

        let t = resolve("openrouter/x", &ch, &builtin_cfg()).unwrap();
        assert_eq!(t.base_url, "https://my-proxy.local/v1");
    }

    #[test]
    fn disabled_channel_is_rejected() {
        let mut ch = HashMap::new();
        let mut c = ChannelConfig::new("groq");
        c.enabled = false;
        c.api_key = Some("sk".into());
        ch.insert("groq".into(), c);

        let err = resolve("groq/llama-3.1-8b", &ch, &builtin_cfg()).unwrap_err();
        assert!(err.message().contains("已被禁用"));
    }

    #[test]
    fn model_overrides_are_enforced() {
        let mut ch = HashMap::new();
        let mut c = ChannelConfig::new("groq");
        c.api_key = Some("sk".into());
        c.model_overrides = Some(vec!["llama-3.1-8b".into()]);
        ch.insert("groq".into(), c);

        assert!(resolve("groq/llama-3.1-8b", &ch, &builtin_cfg()).is_ok());
        let err = resolve("groq/other-model", &ch, &builtin_cfg()).unwrap_err();
        assert!(err.message().contains("模型清单"));
    }

    #[test]
    fn builtin_uses_default_config() {
        let t = resolve("builtin/deepseek-chat", &HashMap::new(), &builtin_cfg()).unwrap();
        assert_eq!(t.provider.id, BUILTIN_ID);
        assert_eq!(t.model, "deepseek-chat");
        assert_eq!(t.base_url, "https://api.deepseek.com/v1");
        assert_eq!(t.api_key.as_deref(), Some("sk-builtin"));
    }

    #[test]
    fn unknown_provider_string_resolves_as_builtin_model() {
        // 整串（含斜杠）都当模型名
        let t = resolve("my-gw/gpt-4", &HashMap::new(), &builtin_cfg()).unwrap();
        assert_eq!(t.provider.id, BUILTIN_ID);
        assert_eq!(t.model, "my-gw/gpt-4");
    }

    #[test]
    fn removed_oauth_provider_falls_back_to_builtin() {
        // gemini 已从注册表移除：`gemini/xxx` 会被当作未知 provider，
        // 回退到 builtin 通道（用老配置的 base_url + model），而不是报「不驱动」。
        let t = resolve("gemini/gemini-2.0-flash", &HashMap::new(), &builtin_cfg());
        match t {
            Ok(target) => {
                // 回退 builtin 后 model 是整串（含 `gemini/` 前缀）
                assert!(
                    target.provider.id == "builtin" || target.provider.id == "gemini",
                    "未知 provider 应回退 builtin，实际 {}",
                    target.provider.id
                );
            }
            Err(e) => {
                // 若老配置没填 base_url/model，则会报配置错误（也合理）
                assert!(
                    !e.message().contains("不驱动"),
                    "不应再出现「不驱动」文案：{}",
                    e.message()
                );
            }
        }
    }

    #[test]
    fn provider_without_base_needs_override() {
        let err = resolve("azure-openai/gpt-4o", &HashMap::new(), &builtin_cfg()).unwrap_err();
        assert!(err.message().contains("手工填写"));

        let mut ch = HashMap::new();
        let mut c = ChannelConfig::new("azure-openai");
        c.api_key = Some("sk".into());
        c.base_url_override = Some("https://contoso.openai.azure.com".into());
        ch.insert("azure-openai".into(), c);
        assert!(resolve("azure-openai/gpt-4o", &ch, &builtin_cfg()).is_ok());
    }

    #[test]
    fn empty_model_is_rejected() {
        let err = resolve("", &HashMap::new(), &builtin_cfg()).unwrap_err();
        assert!(err.message().contains("模型标识为空"));
        let err = resolve("ollama/", &HashMap::new(), &builtin_cfg()).unwrap_err();
        assert!(err.message().contains("模型标识为空"));
    }

    // ───────────── Combo 顺序 ─────────────

    /// 记录调用顺序的假 runner。
    struct FakeRunner {
        /// 按 target 的 `provider/model` 决定成功或失败。
        fail_on: Vec<String>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeRunner {
        fn new(fail_on: &[&str]) -> Self {
            Self {
                fail_on: fail_on.iter().map(|s| s.to_string()).collect(),
                calls: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl ChatRunner for FakeRunner {
        async fn run(&self, target: &ResolvedTarget) -> AppResult<String> {
            let key = format!("{}/{}", target.provider.id, target.model);
            self.calls.lock().unwrap().push(key.clone());
            if self.fail_on.iter().any(|f| f == &key) {
                Err(AppError::Llm(format!("{key} 故意失败")))
            } else {
                Ok(format!("reply-from-{key}"))
            }
        }
    }

    #[tokio::test]
    async fn combo_stops_at_first_success_in_order() {
        let combo = Combo::new(
            "daily",
            vec![
                "ollama/qwen2.5:7b".into(),
                "openrouter/anthropic/claude-3.5".into(),
                "builtin/deepseek-chat".into(),
            ],
        );
        let ch = channels_with(&[("openrouter", "sk-or")]);
        let runner = FakeRunner::new(&[]); // 全部成功

        let report = run_combo_chain(&combo, &ch, &builtin_cfg(), &runner).await;

        assert_eq!(report.winner, Some(0), "第一个成功即返回");
        assert_eq!(report.output(), Some("reply-from-ollama/qwen2.5:7b"));
        assert!(!report.degraded());
        // 关键：成功后不得继续尝试后续成员
        assert_eq!(report.attempts.len(), 1, "成功后应立即停止，不再调用后续成员");
        assert_eq!(runner.calls(), vec!["ollama/qwen2.5:7b".to_string()]);
    }

    #[tokio::test]
    async fn combo_falls_back_in_declared_order() {
        let combo = Combo::new(
            "daily",
            vec![
                "ollama/qwen2.5:7b".into(),
                "openrouter/anthropic/claude-3.5".into(),
                "builtin/deepseek-chat".into(),
            ],
        );
        let ch = channels_with(&[("openrouter", "sk-or")]);
        let runner = FakeRunner::new(&["ollama/qwen2.5:7b", "openrouter/anthropic/claude-3.5"]);

        let report = run_combo_chain(&combo, &ch, &builtin_cfg(), &runner).await;

        assert_eq!(report.winner, Some(2));
        assert!(report.degraded(), "成功者不是第一个 → 发生过降级");
        assert_eq!(
            runner.calls(),
            vec![
                "ollama/qwen2.5:7b".to_string(),
                "openrouter/anthropic/claude-3.5".to_string(),
                "builtin/deepseek-chat".to_string(),
            ],
            "尝试顺序必须严格等于成员声明顺序"
        );
        assert_eq!(report.attempts.len(), 3);
        assert_eq!(report.output(), Some("reply-from-builtin/deepseek-chat"));
    }

    #[tokio::test]
    async fn combo_records_resolution_failures_and_continues() {
        // 第一成员缺 Key / 第二节成员端点未配置 —— 都应计为失败并继续
        let combo = Combo::new(
            "resilient",
            vec![
                "openrouter/anthropic/claude-3.5".into(), // 无 Key → resolve 失败
                "azure-openai/gpt-4o".into(),             // 无 base_url → resolve 失败
                "ollama/qwen2.5:7b".into(),               // 成功
            ],
        );
        let runner = FakeRunner::new(&[]);
        let report = run_combo_chain(&combo, &HashMap::new(), &builtin_cfg(), &runner).await;

        assert_eq!(report.winner, Some(2));
        assert_eq!(report.attempts[0].error.as_deref().unwrap().contains("需要 API Key"), true);
        assert!(report.attempts[1]
            .error
            .as_deref()
            .unwrap()
            .contains("手工填写"));
        // 解析失败不进入 runner
        assert_eq!(runner.calls(), vec!["ollama/qwen2.5:7b".to_string()]);
    }

    #[tokio::test]
    async fn combo_all_fail_reports_every_attempt() {
        let combo = Combo::new(
            "doomed",
            vec!["ollama/qwen2.5:7b".into(), "openrouter/x".into()],
        );
        let ch = channels_with(&[("openrouter", "sk-or")]);
        let runner = FakeRunner::new(&["ollama/qwen2.5:7b", "openrouter/x"]);
        let report = run_combo_chain(&combo, &ch, &builtin_cfg(), &runner).await;

        assert_eq!(report.winner, None);
        assert_eq!(report.output(), None);
        assert_eq!(report.attempts.len(), 2);
        let err = report.to_error();
        assert!(err.message().contains("降级链全部失败"));
        assert!(err.message().contains("ollama/qwen2.5:7b"));
        assert!(err.message().contains("openrouter/x"));
    }

    #[tokio::test]
    async fn empty_combo_attempts_nothing() {
        let combo = Combo::new("empty", vec![]);
        let runner = FakeRunner::new(&[]);
        let report = run_combo_chain(&combo, &HashMap::new(), &builtin_cfg(), &runner).await;
        assert_eq!(report.winner, None);
        assert!(report.attempts.is_empty());
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn combo_id_parsing_and_lookup() {
        let combos = vec![
            Combo::new("daily", vec!["ollama/qwen2.5:7b".into()]),
            Combo::new("fast", vec!["groq/llama-3.1-8b".into()]),
        ];
        assert_eq!(parse_combo_id("combo:daily"), Some("daily"));
        assert_eq!(parse_combo_id("combo:  "), None);
        assert_eq!(parse_combo_id("ollama/x"), None);

        assert_eq!(combos[0].qualified(), "combo:daily");
        assert!(combos[0].matches("combo:daily"));
        assert!(!combos[0].matches("combo:fast"));
        assert_eq!(find_combo("combo:fast", &combos).map(|c| c.name.as_str()), Some("fast"));
        assert!(find_combo("combo:missing", &combos).is_none());
    }

    #[test]
    fn resolve_preview_picks_first_member() {
        let combos = vec![Combo::new(
            "daily",
            vec!["ollama/qwen2.5:7b".into(), "openrouter/x".into()],
        )];
        let t = resolve_preview("combo:daily", &HashMap::new(), &combos, &builtin_cfg()).unwrap();
        assert_eq!(t.provider.id, "ollama", "预览应是首个成员");
        assert_eq!(t.model, "qwen2.5:7b");
    }

    #[test]
    fn strategy_only_fallback() {
        assert_eq!(ComboStrategy::parse("fallback").unwrap(), ComboStrategy::Fallback);
        assert_eq!(ComboStrategy::parse("").unwrap(), ComboStrategy::Fallback);
        assert_eq!(ComboStrategy::parse("Fallback").unwrap(), ComboStrategy::Fallback);
        // 契约 §1.2：round-robin 明确不做
        assert!(ComboStrategy::parse("round-robin").is_err());
    }

    #[test]
    fn resolves_kilo_as_free_channel_without_key() {
        // Kilo 是免 Key 通道：即使配了 Key 也不该带上（AuthKind::None 会丢弃）
        let ch = channels_with(&[("kilo", "kilo-key-should-be-ignored")]);
        let t = resolve("kilo/kilo-auto/free", &ch, &builtin_cfg()).unwrap();
        assert_eq!(t.provider.category, ProviderCategory::Free);
        assert_eq!(t.base_url, registry::KILO_GATEWAY_BASE);
        assert_eq!(t.api_key, None, "免 Key 通道不得把 Key 发往上游");
        // model_id 里的斜杠要保留
        assert_eq!(t.model, "kilo-auto/free");
    }

    #[test]
    fn resolved_target_display_hides_api_key() {
        let ch = channels_with(&[("openrouter", "sk-secret-value")]);
        let t = resolve("openrouter/x", &ch, &builtin_cfg()).unwrap();
        let shown = t.to_string();
        assert!(!shown.contains("sk-secret-value"), "日志输出不得泄露 Key：{shown}");
        assert!(shown.contains("openrouter/x"));
        assert!(shown.contains("https://openrouter.ai/api/v1"));
    }

    #[test]
    fn unknown_provider_resolve_keeps_model_intact() {
        // 注册表里确实没有的 provider id
        assert!(find("totally-made-up").is_none());
        let (p, m) = parse_model_id("totally-made-up/some-model");
        assert_eq!(p, BUILTIN_ID);
        assert_eq!(m, "totally-made-up/some-model");
    }
}
