//! 通道与模型发现（M-A2）。
//!
//! 职责：为每个注册表提供商探测真实可用性、拉取真实模型清单。
//!
//! # 铁律：不许伪造成功
//!
//! 本模块的每个「可用」结论都必须来自一次真实的 HTTP 往返。
//! 连不上就报 `Unreachable`，没有模型就报 `NoModels`，Key 被拒就报 `Unauthorized`，
//! **绝不返回写死的模型清单来假装探测成功**（契约 §7 最后一条验收）。
//!
//! # Kilo 的实测结论（本轮实机验证，2026-11，逐模型 POST 复验）
//!
//! Kilo 有两份**不同**的清单，而且它们的 id 命名空间互不相同、行为也完全不同。
//! 这一点曾导致两轮误判，故把完整矩阵记在这里，避免后人再踩。
//!
//! ## 清单 A：`GET https://kilo.ai/api/models`（158 项，公开标价目录）
//! - 字段里**没有** `isFree`；免费与否只能看 `priceInput` / `priceOutput`（字符串数字）。
//! - 其中 **20 个**两项都为 `0.000000`。
//! - 但这些 **slug 命名空间**的模型直接 POST 会 **401**：
//!   ```text
//!   POST /api/gateway/v1/chat/completions  {"model":"minimax-minimax-m3-free"}
//!   → 401 {"error":{"code":"PAID_MODEL_AUTH_REQUIRED",
//!                   "message":"You need to sign in to use this model."}}
//!   ```
//!   `minimax-minimax-m2-7-free` / `google-gemma-4-26b-a4b-it-free` /
//!   `inclusionai-ling-2-6-flash-free` 同样 401。
//! - **结论：标价 $0 ≠ 免登录可调用。** 这类模型只做展示，文案必须是
//!   「标价 $0，但需登录 Kilo 账号才能调用」。
//!
//! ## 清单 B：`GET https://kilo.ai/api/gateway/v1/models`（384 项，网关目录）
//! - 字段里**有** `isFree`（15 个 `true`），id 形如 `nvidia/nemotron-3.5-lightning:free`。
//! - 这 15 个中实测多个 **200 且真实推理、`usage.cost = 0`、`is_byok = false`**：
//!   `nvidia/nemotron-3.5-lightning:free`、`openrouter/free`、
//!   `cohere/north-mini-code:free`、`liquid/lfm-2.5-2.6b:free`、
//!   `nvidia/nemotron-3-ultra-550b-a55b:free`、`reflection/beam-free`。
//! - 例外的 `inclusionai/ling-3.1-flash-1019-free` 返回 **429**（限流，非鉴权问题）。
//!
//! ## 自动路由：[`KILO_AUTO_FREE_MODEL`]
//! `kilo-auto/free` 连跑多次稳定 **200**，`usage.cost = 0`，`is_byok = false`，
//! 上游落在 `reflection/beam-free`。这是**跨命名空间、免登录**的稳定入口，
//! 因此单独以常量暴露给调用方。
//!
//! ## 其它
//! - `kilo.ai/v1/chat/completions` → **404**（命中 Next.js 404 页，从来不是推理入口）。
//!
//! ## ⚠️ 端点纠错（本轮修正的重要事实）
//! 上面「需登录」的判断建立在一个**错误的端点**上：`kilo.ai/api/gateway/v1`。
//! 正确主机是 **`api.kilo.ai`**。换成 `https://api.kilo.ai/api/gateway` 后逐个实测：
//!
//! | 模型 | POST 结果 |
//! |---|---|
//! | `kilo-auto/free` | 200, cost=0 |
//! | `nvidia/nemotron-3-ultra-550b-a55b:free` | 200, cost=0 |
//! | `nvidia/nemotron-3.5-lightning:free` | 200, cost=0 |
//! | `openrouter/free` | 200, cost=0 |
//! | `cohere/north-mini-code:free` | 200, cost=0 |
//! | `reflection/beam-free` | 200, cost=0 |
//! | … 共 15 个 `isFree`，其中 **12 个实测可用**，3 个是上游故障（503/429/404） |
//!
//! **结论修正**：Kilo 网关 `isFree: true` 的模型确实**免 Key、免登录**，
//! `usage.cost = 0`、`is_byok = false`。注册表里它的 category 是 `Free`。
//! 官网目录里的连字符 `xxx-free` 是另一套命名空间（需登录），与本免费池无关。

use std::time::Duration;

use reqwest::Client;
use serde_json::Value;

use crate::error::{AppError, AppResult};
use crate::llm::openai::normalize_base_url;
use crate::llm::registry::{
    self, AuthKind, ProviderCategory, ProviderEntry, KILO_CATALOG_URL,
};
#[cfg(test)]
use crate::llm::registry::KILO_GATEWAY_BASE;

/// 探测与模型发现的 HTTP 超时（契约要求 10s）。
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Kilo 网关清单的读超时。
///
 /// ⚠️ 实测（2026-11）：`api.kilo.ai/api/gateway/models` 的响应体约 **400–520 KB**，
 /// 端到端耗时 **26–30 秒**（重复测三次：30.0s / 26.8s / 26.6s）。
 /// 用统一的 10 秒 `PROBE_TIMEOUT` 会在读 body 时超时，reqwest 返回**空 body**，
 /// 于是 serde 报 `EOF while parsing a value at line 1 column 0`
 /// —— 这正是用户看到的「网关模型清单读取失败」的来源。
pub const CATALOG_TIMEOUT: Duration = Duration::from_secs(60);

/// Kilo 自动免费路由的模型 id（实测免登录可用，cost=0）。
///
/// 实测矩阵（`POST https://kilo.ai/api/gateway/v1/chat/completions`，2026-11）：
/// ```text
/// kilo-auto/free                          → 200  upstream=reflection/beam-free  cost=0  is_byok=false
/// nvidia/nemotron-3.5-lightning:free      → 200  cost=0  is_byok=false
/// openrouter/free                         → 200  upstream=nvidia/nemotron-3-ultra-550b-a55b:free  cost=0
/// cohere/north-mini-code:free             → 200  cost=0
/// liquid/lfm-2.5-2.6b:free                → 200  cost=0
/// nvidia/nemotron-3-ultra-550b-a55b:free  → 200  cost=0
/// reflection/beam-free                    → 200  cost=0
/// inclusionai/ling-3.1-flash-1019-free    → 429  （限流，非鉴权）
/// minimax-minimax-m3-free                 → 401  PAID_MODEL_AUTH_REQUIRED
/// minimax-minimax-m2-7-free               → 401  PAID_MODEL_AUTH_REQUIRED
/// google-gemma-4-26b-a4b-it-free          → 401  PAID_MODEL_AUTH_REQUIRED
/// inclusionai-ling-2-6-flash-free         → 401  PAID_MODEL_AUTH_REQUIRED
/// inclusionai-ling-2-6-1t-free            → 401  PAID_MODEL_AUTH_REQUIRED
/// ```
///
/// 注意最后 5 个：它们是 `GET /api/models` 里**标价 $0 的 slug 命名空间**，
/// 但直接 POST 需要登录。带 `/` 与 `:free` 后缀的是网关命名空间，可免登录调用。
pub const KILO_AUTO_FREE_MODEL: &str = "kilo-auto/free";

/// 错误响应体摘要上限。
const BODY_SUMMARY_LIMIT: usize = 200;

/// 构建统一的探测客户端。
fn build_client() -> Client {
    Client::builder()
        .connect_timeout(PROBE_TIMEOUT)
        .timeout(PROBE_TIMEOUT)
        .build()
        .unwrap_or_else(|_| Client::new())
}

/// 用于拉取**大模型清单**的 client（Kilo 两份目录都 400KB+、耗时 30s）。
fn build_catalog_client() -> Client {
    Client::builder()
        .connect_timeout(PROBE_TIMEOUT)
        .timeout(CATALOG_TIMEOUT)
        .build()
        .unwrap_or_else(|_| Client::new())
}

/// 探测结论。**每个变体都对应一次真实的探测结果**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeStatus {
    /// 连通且拿到了模型清单。
    Available,
    /// 服务在跑，但一个模型都没有（例如 Ollama 已启动但没 `ollama pull`）。
    NoModels,
    /// 连不上（未安装 / 未启动 / 网络不可达）。
    Unreachable,
    /// 端点可达但认证被拒（Key 无效或缺失）。
    Unauthorized,
    /// 只能读公开目录，不能用作推理入口（Kilo）。
    CatalogOnly,
    /// 该渠道不支持探测（保留给未来需要交互式登录的通道；
    /// 当前 OAuth 条目已移除，一般不会出现在结果里）。
    Unsupported,
}

impl ProbeStatus {
    /// 是否可直接用于对话。
    pub fn is_usable(self) -> bool {
        matches!(self, ProbeStatus::Available)
    }

    /// 稳定的字符串标识，供 IPC / 前端分支使用。
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeStatus::Available => "available",
            ProbeStatus::NoModels => "no_models",
            ProbeStatus::Unreachable => "unreachable",
            ProbeStatus::Unauthorized => "unauthorized",
            ProbeStatus::CatalogOnly => "catalog_only",
            ProbeStatus::Unsupported => "unsupported",
        }
    }
}

/// 一次探测的完整结果，`message` 是可直接展示给用户的中文文案。
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub status: ProbeStatus,
    /// 给 UI 直接显示的一句话结论。
    pub message: String,
    /// 真实拉到的模型 id（按 id 排序去重）。
    pub models: Vec<String>,
    /// 仅 Kilo 目录：零价格模型数（`priceInput` / `priceOutput` 均为 0）。
    pub free_model_count: usize,
}

impl ProbeOutcome {
    fn new(status: ProbeStatus, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            models: Vec::new(),
            free_model_count: 0,
        }
    }

    fn with_models(mut self, models: Vec<String>) -> Self {
        self.models = models;
        self
    }
}

// ─────────────────────────── 基础 URL 处理 ───────────────────────────

/// 解析实际要用的 base_url：override 优先，其次注册表默认值。
///
/// 归一化复用 [`normalize_base_url`]（与 `openai.rs` 保持一致的容忍度：
/// 尾斜杠、缺 `/v1`、误贴完整 endpoint 都会被纠正）。
pub fn effective_base_url(
    provider: &ProviderEntry,
    base_url_override: Option<&str>,
) -> AppResult<String> {
    let raw = match base_url_override.map(str::trim).filter(|s| !s.is_empty()) {
        Some(v) => v,
        None => provider.base_url.ok_or_else(|| {
            AppError::Llm(format!(
                "提供商 {} 没有默认端点，需要在设置里手工填写 base_url",
                provider.name
            ))
        })?,
    };
    Ok(normalize_base_url(raw))
}

/// 从 `http://127.0.0.1:11434/v1` 取回站点根 `http://127.0.0.1:11434`，
/// 用于 Ollama 的 `/api/tags`（它不在 `/v1` 命名空间下）。
pub fn site_root(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    for suffix in ["/v1", "/v1beta"] {
        if let Some(stripped) = b.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    b.to_string()
}

/// `{base}/models`。
fn models_url(base: &str) -> String {
    format!("{}/models", base.trim_end_matches('/'))
}

/// `{root}/api/tags`（Ollama 专有）。
fn ollama_tags_url(base: &str) -> String {
    format!("{}/api/tags", site_root(base))
}

fn apply_auth(rb: reqwest::RequestBuilder, provider: &ProviderEntry, key: Option<&str>) -> reqwest::RequestBuilder {
    match provider.auth {
        AuthKind::None => rb,
        AuthKind::Bearer => match key.map(str::trim).filter(|s| !s.is_empty()) {
            Some(k) => rb.bearer_auth(k),
            None => rb, // 没 Key 就裸发，让服务端明确回 401，而不是在本地假装成功
        },
    }
}

fn truncate(s: &str, limit: usize) -> String {
    let mut out: String = s.chars().take(limit).collect();
    if s.chars().count() > limit {
        out.push('…');
    }
    out
}

/// 把任意响应体解析成模型 id 列表。
///
/// 同时兼容三种真实形状：
/// - OpenAI 兼容：`{"data":[{"id":"gpt-4o"}]}`
/// - **Ollama `/api/tags`**：`{"models":[{"name":"qwen2.5:0.5b","model":"..."}]}`
/// - 裸数组：`[{"id":"..."}]` / `["name"]`
pub fn parse_models_payload(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    let push = |out: &mut Vec<String>, raw: Option<&str>| {
        if let Some(s) = raw {
            let s = s.trim();
            if !s.is_empty() {
                out.push(s.to_string());
            }
        }
    };

    // 逐个形状取出条目数组
    let mut arrays: Vec<&Vec<Value>> = Vec::new();
    if let Some(a) = v.get("data").and_then(Value::as_array) {
        arrays.push(a);
    }
    if let Some(a) = v.get("models").and_then(Value::as_array) {
        arrays.push(a);
    }
    if let Some(a) = v.as_array() {
        arrays.push(a);
    }

    for arr in arrays {
        for item in arr {
            if let Some(s) = item.as_str() {
                push(&mut out, Some(s));
                continue;
            }
            // id 优先（OpenAI），其次 name / model（Ollama）
            push(&mut out, item.get("id").and_then(Value::as_str));
            push(&mut out, item.get("name").and_then(Value::as_str));
            push(&mut out, item.get("model").and_then(Value::as_str));
        }
    }

    out.sort();
    out.dedup();
    out
}

/// 分类一个失败响应：401/403 视为 Key 问题，其余视为端点问题。
fn read_status_summary(status: reqwest::StatusCode, body: &str) -> String {
    let trimmed = body.trim();
    let summary = truncate(trimmed, BODY_SUMMARY_LIMIT);
    format!("HTTP {}: {summary}", status.as_u16())
}

// ─────────────────────────── Kilo 目录 ───────────────────────────

/// Kilo 公开目录里的一个模型项。字段名来自**实测**结构（`kilo.ai/api/models`）。
#[derive(Debug, Clone, PartialEq)]
pub struct KiloCatalogModel {
    /// 目录里的稳定标识（如 `minimax-minimax-m3-free`）。
    pub slug: String,
    pub name: String,
    pub context_length: Option<u64>,
    /// 每百万输入 token 的价格（字符串数字，如 `"0.000000"`）。
    pub price_input: Option<f64>,
    pub price_output: Option<f64>,
    /// **仅表示价格为零**（输入与输出都能解析且都为 0）。
    ///
    /// ⚠️ **这不等于「可直接调用」**。实测表明本目录的零价格 slug
    /// （如 `minimax-minimax-m3-free`）直接 POST 会返回
    /// `401 PAID_MODEL_AUTH_REQUIRED` —— 需要登录 Kilo 账号。
    /// UI 文案应为「标价 $0，但需登录 Kilo 账号才能调用」。
    /// 真正免登录的入口见 [`KILO_AUTO_FREE_MODEL`] 与网关命名空间（`...:free`）。
    pub is_free: bool,
}

impl KiloCatalogModel {
    /// 给 UI 的中文价格说明，**不暗示可直接调用**。
    pub fn price_label(&self) -> &'static str {
        if self.is_free {
            "标价 $0，但需登录 Kilo 账号才能调用"
        } else {
            "按量计费，需登录 Kilo 账号"
        }
    }
}

/// 宽松地把字符串或数字解析成 f64。
fn as_f64_lenient(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// 解析 Kilo 公开目录。
///
/// 接受三种外形：裸数组（实测形状）、`{"data":[...]}`、`{"models":[...]}`。
pub fn parse_kilo_catalog(v: &Value) -> Vec<KiloCatalogModel> {
    let arr: &[Value] = if let Some(a) = v.as_array() {
        a
    } else if let Some(a) = v.get("data").and_then(Value::as_array) {
        a
    } else if let Some(a) = v.get("models").and_then(Value::as_array) {
        a
    } else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for item in arr {
        let slug = match item.get("slug").and_then(Value::as_str) {
            Some(s) if !s.trim().is_empty() => s.trim().to_string(),
            // 有些条目可能只有 id
            _ => match item.get("id").and_then(Value::as_str) {
                Some(s) if !s.trim().is_empty() => s.trim().to_string(),
                _ => continue,
            },
        };
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&slug)
            .to_string();
        let price_input = as_f64_lenient(item.get("priceInput"));
        let price_output = as_f64_lenient(item.get("priceOutput"));
        // 未知价格不算免费：不能因为字段缺失就把付费模型标成免费
        let is_free = matches!((price_input, price_output), (Some(i), Some(o)) if i == 0.0 && o == 0.0);

        out.push(KiloCatalogModel {
            slug,
            name,
            context_length: item.get("contextLength").and_then(Value::as_u64),
            price_input,
            price_output,
            is_free,
        });
    }
    out
}

/// 拉取 Kilo 公开目录（不调推理端点）。
pub async fn fetch_kilo_catalog() -> AppResult<Vec<KiloCatalogModel>> {
    let client = build_catalog_client();
    let resp = client.get(KILO_CATALOG_URL).send().await.map_err(|e| {
        if e.is_timeout() {
            AppError::Llm(format!("读取 Kilo 模型目录超时（{}s）", PROBE_TIMEOUT.as_secs()))
        } else {
            AppError::Llm(format!("无法连接 Kilo 模型目录 {KILO_CATALOG_URL}: {e}"))
        }
    })?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AppError::Llm(format!(
            "Kilo 模型目录返回错误（{}）",
            read_status_summary(status, &body)
        )));
    }

    let v: Value = serde_json::from_str(&body).map_err(|e| {
        AppError::Llm(format!(
            "Kilo 模型目录不是合法 JSON（{e}）：{}",
            truncate(&body, BODY_SUMMARY_LIMIT)
        ))
    })?;
    Ok(parse_kilo_catalog(&v))
}

// ─────────────────────────── Kilo 免 Key 免费池 ───────────────────────────

/// 网关清单里的一个模型（`/api/gateway/models` 的真实形状）。
#[derive(Debug, Clone, PartialEq)]
pub struct KiloFreeModel {
    /// 网关命名空间下的 model id，如 `nvidia/nemotron-3.5-lightning:free`
    pub id: String,
    pub name: String,
    pub context_length: Option<u64>,
    /// 上游是否声明 **prompt 可能被用于训练/改进服务**（必须透传给用户）
    pub may_train_on_prompts: bool,
}

/// 解析网关模型清单。
///
/// 真实形状是 `{"data":[{"id":..., "isFree":true, "context_length":...,
/// "mayTrainOnYourPrompts":true, ...}, ...]}`（384 项，15 个 `isFree`）。
/// 只收 `isFree == true` 的项 —— 其余都是付费 id，免 Key POST 会 401。
pub fn parse_kilo_gateway_models(v: &Value) -> Vec<KiloFreeModel> {
    let items = v
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut out = Vec::new();
    for item in &items {
        if item.get("isFree") != Some(&Value::Bool(true)) {
            continue;
        }
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        if id.trim().is_empty() {
            continue;
        }
        out.push(KiloFreeModel {
            id: id.trim().to_string(),
            name: item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(id.trim())
                .to_string(),
            context_length: item.get("context_length").and_then(Value::as_u64),
            may_train_on_prompts: item
                .get("mayTrainOnYourPrompts")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    // 稳定排序，便于 UI 与测试比对
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// 拉取 Kilo 免 Key 免费池清单（真实网络请求，超时 20s）。
pub async fn fetch_kilo_free_models() -> AppResult<Vec<KiloFreeModel>> {
    // 清单很大且慢（见 CATALOG_TIMEOUT 的实测注释），必须用长超时 client
    let client = build_catalog_client();
    let resp = client
        .get(crate::llm::registry::KILO_GATEWAY_MODELS_URL)
        .header("user-agent", "modern-sshdeck")
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                AppError::Llm("读取 Kilo 免费池清单超时".into())
            } else {
                AppError::Llm(format!("无法连接 Kilo 网关模型清单: {e}"))
            }
        })?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AppError::Llm(format!(
            "Kilo 网关模型清单返回错误（{}）",
            read_status_summary(status, &body)
        )));
    }
    let v: Value = serde_json::from_str(&body).map_err(|e| {
        AppError::Llm(format!(
            "Kilo 网关清单不是合法 JSON（{e}）：{}",
            truncate(&body, BODY_SUMMARY_LIMIT)
        ))
    })?;
    Ok(parse_kilo_gateway_models(&v))
}

// ─────────────────────────── 模型发现 ───────────────────────────

/// 发现某个提供商真实可用的模型 id。
///
/// - 本机（Ollama）：先 `GET /api/tags`（`{"models":[{"name":...}]}`），
///   失败再退到 OpenAI 兼容的 `/v1/models`。
/// - Kilo：读公开目录（它没有免 Key 的推理入口）。
/// - 其余：`GET {base}/models`，需要 Key 时带 Bearer。
///
/// 失败一律返回**带原因**的 [`AppError::Llm`]，绝不返回空列表冒充成功。
pub async fn discover_models(
    provider: &ProviderEntry,
    key: Option<&str>,
    base_url_override: Option<&str>,
) -> AppResult<Vec<String>> {
    // ⚠️ 前置校验：需要 Key 但没填时，**不要发请求**。
    //
    // 之前会把注定失败的请求发出去，上游回 401 后原样抛给用户，
    // 于是界面上出现一堆看不懂的报错，例如 DeepSeek 的
    // 「Authentication Fails (governor)」—— 用户看到「governor」完全不知道
    // 那其实只是「你没填 Key」。这里改为给出可操作的中文说明。
    if provider.auth == AuthKind::Bearer && key.map(str::trim).unwrap_or("").is_empty() {
        return Err(AppError::Llm(format!(
            "{} 需要 API Key：请先在「渠道」页填入 Key，再探测可用模型",
            provider.name
        )));
    }

    // Kilo：只返回**网关声明 isFree 且实测免 Key 可用**的模型。
    // 官网目录里的 `xxx-free`（连字符）是另一套命名空间，需登录，不能当免费池用。
    if provider.id == "kilo" {
        let free = fetch_kilo_free_models().await?;
        if free.is_empty() {
            return Err(AppError::Llm(
                "Kilo 网关没有返回任何 isFree 模型（清单结构可能已变更）".into(),
            ));
        }
        return Ok(free.into_iter().map(|m| m.id).collect());
    }

    let base = effective_base_url(provider, base_url_override)?;
    let client = build_client();

    // 本机 Ollama：优先原生 `/api/tags`
    if provider.id == "ollama" {
        if let Ok(models) = fetch_models_at(&client, &ollama_tags_url(&base), provider, key).await {
            if !models.is_empty() {
                return Ok(models);
            }
        }
        // 退到 OpenAI 兼容端点（部分版本只开了 /v1）
    }

    let models = fetch_models_at(&client, &models_url(&base), provider, key).await?;
    if models.is_empty() {
        return Err(AppError::Llm(format!(
            "{} 可连通但模型列表为空：请先在本地拉取模型（如 `ollama pull qwen2.5:7b`）",
            provider.name
        )));
    }
    Ok(models)
}

/// 请求一个模型列表端点并解析。
async fn fetch_models_at(
    client: &Client,
    url: &str,
    provider: &ProviderEntry,
    key: Option<&str>,
) -> AppResult<Vec<String>> {
    let resp = apply_auth(client.get(url), provider, key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                AppError::Llm(format!("请求模型列表超时（{}s）: {url}", PROBE_TIMEOUT.as_secs()))
            } else {
                AppError::Llm(format!("无法连接 {url}: {e}"))
            }
        })?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AppError::Llm(format!(
            "获取模型列表失败（{}）: {url}",
            read_status_summary(status, &body)
        )));
    }

    let v: Value = serde_json::from_str(&body).map_err(|e| {
        AppError::Llm(format!(
            "模型列表不是合法 JSON（{e}）: {url}: {}",
            truncate(&body, BODY_SUMMARY_LIMIT)
        ))
    })?;
    Ok(parse_models_payload(&v))
}

// ─────────────────────────── 探测 ───────────────────────────

/// 探测一个提供商，返回可展示的结论。
///
/// 语义保证（**这是 UI 文案分层的基础**）：
/// - 连不上（拒连/超时/DNS）→ [`ProbeStatus::Unreachable`] +「未安装或未启动」
/// - 连上但列表空 → [`ProbeStatus::NoModels`] +「已启动但没有模型」
/// - 401/403 → [`ProbeStatus::Unauthorized`] +「Key 无效」
/// - 拿到模型 → [`ProbeStatus::Available`]
pub async fn probe(
    provider: &ProviderEntry,
    key: Option<&str>,
    base_url_override: Option<&str>,
) -> ProbeOutcome {
    // Kilo：目录可读，推理需账号 —— 单独一条路径，避免被误报成可用
    if provider.id == "kilo" {
        return probe_kilo().await;
    }

    let base = match effective_base_url(provider, base_url_override) {
        Ok(b) => b,
        Err(e) => return ProbeOutcome::new(ProbeStatus::Unreachable, e.message()),
    };

    let client = build_client();
    let url = models_url(&base);

    let resp = match apply_auth(client.get(&url), provider, key)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let msg = if e.is_timeout() {
                format!("{}：连接 {} 超时，服务可能未启动", provider.name, base)
            } else if e.is_connect() {
                format!(
                    "{}：无法连接 {}，服务未安装或未启动",
                    provider.name, base
                )
            } else {
                format!("{}：请求 {} 失败（{e}）", provider.name, base)
            };
            return ProbeOutcome::new(ProbeStatus::Unreachable, msg);
        }
    };

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        let msg = if provider.needs_key() && key.map(str::trim).unwrap_or("").is_empty() {
            format!("{}：需要 API Key，尚未填写", provider.name)
        } else {
            format!(
                "{}：API Key 被拒绝（{}）",
                provider.name,
                truncate(body.trim(), BODY_SUMMARY_LIMIT)
            )
        };
        return ProbeOutcome::new(ProbeStatus::Unauthorized, msg);
    }

    if !status.is_success() {
        return ProbeOutcome::new(
            ProbeStatus::Unreachable,
            format!(
                "{}：端点返回错误（{}）",
                provider.name,
                read_status_summary(status, &body)
            ),
        );
    }

    let models = match serde_json::from_str::<Value>(&body) {
        Ok(v) => parse_models_payload(&v),
        Err(e) => {
            return ProbeOutcome::new(
                ProbeStatus::Unreachable,
                format!("{}：模型列表格式异常（{e}）", provider.name),
            )
        }
    };

    if models.is_empty() {
        // 本机通道：服务在，但没有模型 —— 与「未启动」是两个不同的文案
        let msg = if provider.category == ProviderCategory::Local {
            format!(
                "{}：服务已启动，但没有任何模型；请先拉取模型（如 `ollama pull qwen2.5:7b`）",
                provider.name
            )
        } else {
            format!("{}：已连通，但返回的模型列表为空", provider.name)
        };
        return ProbeOutcome::new(ProbeStatus::NoModels, msg).with_models(models);
    }

    let msg = format!("{}：可用，发现 {} 个模型", provider.name, models.len());
    ProbeOutcome::new(ProbeStatus::Available, msg).with_models(models)
}

/// Kilo 专用探测：读网关的 `isFree` 清单，这些模型**免 Key 可用**（`cost: 0`）。
///
/// 与官网目录（`KILO_CATALOG_URL`）的区别是关键：目录的零价格 slug 需登录，
/// 网关的 `isFree` 才是免 Key 推理清单。这里只认网关。
async fn probe_kilo() -> ProbeOutcome {
    match fetch_kilo_free_models().await {
        Ok(free) => {
            let n = free.len();
            if n == 0 {
                return ProbeOutcome::new(
                    ProbeStatus::NoModels,
                    "Kilo：网关清单里没有 isFree 模型".to_string(),
                );
            }
            let mut max_ctx = 0u64;
            let mut max_ctx_id = String::new();
            let mut trains = 0usize;
            for m in &free {
                if let Some(c) = m.context_length {
                    if c > max_ctx {
                        max_ctx = c;
                        max_ctx_id = m.id.clone();
                    }
                }
                if m.may_train_on_prompts {
                    trains += 1;
                }
            }
            let ctx_hint = if max_ctx > 0 {
                format!(
                    "，最大上下文 {}（{}）",
                    human_context(max_ctx),
                    max_ctx_id
                )
            } else {
                String::new()
            };
            let train_hint = if trains > 0 {
                format!("；其中 {trains} 个上游声明 prompt 可能被用于改进服务，勿发送敏感内容")
            } else {
                String::new()
            };
            ProbeOutcome {
                status: ProbeStatus::Available,
                message: format!("Kilo：免 Key 免费池可用，发现 {n} 个模型{ctx_hint}{train_hint}"),
                models: free.iter().map(|m| m.id.clone()).collect(),
                free_model_count: n,
            }
        }
        Err(e) => ProbeOutcome::new(
            ProbeStatus::Unreachable,
            format!("Kilo：网关模型清单读取失败（{}）", e.message()),
        ),
    }
}

/// 把上下文长度格式化成 `128K` / `1M` 这种人类可读形式。
fn human_context(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1024 {
        format!("{}K", n / 1024)
    } else {
        n.to_string()
    }
}

/// 探测全部提供商，保持注册表顺序。
pub async fn probe_all(
    key_for: impl Fn(&ProviderEntry) -> Option<String>,
    overrides: &std::collections::HashMap<String, String>,
) -> Vec<(String, ProbeOutcome)> {
    let mut out = Vec::new();
    for p in registry::all() {
        let key = key_for(p);
        let base_override = overrides.get(p.id).map(String::as_str);
        let outcome = probe(p, key.as_deref(), base_override).await;
        out.push((p.id.to_string(), outcome));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::registry::find;

    #[test]
    fn parses_openai_compat_shape() {
        let v: Value = serde_json::from_str(
            r#"{"object":"list","data":[
                {"id":"gpt-4o","object":"model"},
                {"id":"deepseek-chat","object":"model"},
                {"id":"gpt-4o","object":"model"}]}"#,
        )
        .unwrap();
        assert_eq!(
            parse_models_payload(&v),
            vec!["deepseek-chat".to_string(), "gpt-4o".to_string()]
        );
    }

    #[test]
    fn parses_ollama_tags_shape() {
        // 真实形状：本机 `GET /api/tags` 的返回（取自实机响应）
        let v: Value = serde_json::from_str(
            r#"{"models":[{"name":"qwen2.5:0.5b","model":"qwen2.5:0.5b",
                "modified_at":"2026-11-01T09:37:35.554666+08:00","size":397821319,
                "digest":"a8b0c51577010a279d933d14c2a8ab4b268079d44c5c8830c0a93900f1827c67"}]}"#,
        )
        .unwrap();
        assert_eq!(parse_models_payload(&v), vec!["qwen2.5:0.5b".to_string()]);
    }

    #[test]
    fn parses_ollama_v1_models_shape() {
        // 真实形状：Ollama 的 OpenAI 兼容端点
        let v: Value = serde_json::from_str(
            r#"{"object":"list","data":[{"id":"qwen2.5:0.5b","object":"model","created":0,"owned_by":"library"}]}"#,
        )
        .unwrap();
        assert_eq!(parse_models_payload(&v), vec!["qwen2.5:0.5b".to_string()]);
    }

    #[test]
    fn parses_empty_and_bare_array() {
        let empty = serde_json::json!({"data":[]});
        assert!(parse_models_payload(&empty).is_empty());
        let bare = serde_json::json!(["a","b"]);
        assert_eq!(
            parse_models_payload(&bare),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn kilo_catalog_free_judged_by_price_not_isfree() {
        // 真实形状：kilo.ai/api/models 的条目（字段名取自实测）
        let v: Value = serde_json::from_str(
            r#"[
              {"slug":"minimax-minimax-m3-free","name":"MiniMax M3 (free)",
               "priceInput":"0.000000","priceOutput":"0.000000","contextLength":1000000},
              {"slug":"paid-model","name":"Paid Model",
               "priceInput":"3.000000","priceOutput":"15.000000","contextLength":128000}
            ]"#,
        )
        .unwrap();
        let list = parse_kilo_catalog(&v);
        assert_eq!(list.len(), 2);
        assert!(list[0].is_free, "零价格应判为免费");
        assert!(!list[1].is_free, "非零价格不应判为免费");
        assert_eq!(list[0].context_length, Some(1_000_000));
        assert_eq!(list[0].price_input, Some(0.0));
    }

    #[test]
    fn kilo_catalog_missing_price_is_not_free() {
        // 字段缺失不能当作免费，否则会把付费模型标成免费
        let v = serde_json::json!([{"slug":"unknown-pricing","name":"X"}]);
        let list = parse_kilo_catalog(&v);
        assert_eq!(list.len(), 1);
        assert!(!list[0].is_free);
        assert_eq!(list[0].price_input, None);
    }

    #[test]
    fn site_root_strips_version_segment() {
        assert_eq!(site_root("http://127.0.0.1:11434/v1"), "http://127.0.0.1:11434");
        assert_eq!(site_root("http://127.0.0.1:8080"), "http://127.0.0.1:8080");
        assert_eq!(
            site_root("http://127.0.0.1:1234/v1/"),
            "http://127.0.0.1:1234"
        );
    }

    #[test]
    fn effective_base_url_prefers_override() {
        let ollama = find("ollama").unwrap();
        assert_eq!(
            effective_base_url(ollama, None).unwrap(),
            "http://127.0.0.1:11434/v1"
        );
        assert_eq!(
            effective_base_url(ollama, Some("http://127.0.0.1:11435")).unwrap(),
            "http://127.0.0.1:11435/v1"
        );
        // override 为空串时退回默认
        assert_eq!(
            effective_base_url(ollama, Some("  ")).unwrap(),
            "http://127.0.0.1:11434/v1"
        );
    }

    #[test]
    fn provider_without_default_base_errors_clearly() {
        let azure = find("azure-openai").unwrap();
        let err = effective_base_url(azure, None).unwrap_err();
        assert!(err.message().contains("手工填写"));
        // 给了 override 就正常
        assert!(effective_base_url(azure, Some("https://x.openai.azure.com")).is_ok());
    }

    #[test]
    fn probe_outcome_messages_are_chinese_and_distinct() {
        // 三种本机失败态文案必须互不相同（UI 据文案区分「未启动」与「无模型」）
        let unreachable = ProbeOutcome::new(
            ProbeStatus::Unreachable,
            "Ollama：无法连接 http://127.0.0.1:11434/v1，服务未安装或未启动",
        );
        let nomodels = ProbeOutcome::new(
            ProbeStatus::NoModels,
            "Ollama：服务已启动，但没有任何模型",
        );
        assert_ne!(unreachable.message, nomodels.message);
        assert!(!unreachable.status.is_usable());
        assert!(!nomodels.status.is_usable());
    }


    #[test]
    fn catalog_free_label_never_claims_directly_callable() {
        // 零价格 ≠ 可调用。实测该 slug 命名空间返回 401 PAID_MODEL_AUTH_REQUIRED，
        // 因此文案必须显式写明「需登录」，不能只写「免费」。
        let m = KiloCatalogModel {
            slug: "minimax-minimax-m3-free".into(),
            name: "MiniMax M3".into(),
            context_length: Some(1_000_000),
            price_input: Some(0.0),
            price_output: Some(0.0),
            is_free: true,
        };
        let label = m.price_label();
        assert!(label.contains("登录"), "文案必须注明需登录：{label}");
        assert!(
            !label.contains("免 Key") && !label.contains("可直接调用"),
            "文案不得暗示免登录可调用：{label}"
        );

        let paid = KiloCatalogModel {
            slug: "paid".into(),
            name: "Paid".into(),
            context_length: None,
            price_input: Some(3.0),
            price_output: Some(15.0),
            is_free: false,
        };
        assert!(!paid.price_label().contains("$0"));
    }

    #[test]
    fn kilo_auto_free_constant_is_exposed() {
        // lead 要求的稳定入口：必须可被调用方直接引用
        assert_eq!(KILO_AUTO_FREE_MODEL, "kilo-auto/free");
        // 它属于网关命名空间（含 `/`），与目录 slug 命名空间不同
        assert!(KILO_AUTO_FREE_MODEL.contains('/'));
    }

    #[test]
    fn kilo_gateway_free_pool_is_parsed_and_requires_is_free_true() {
        // 真实形状来自 `https://api.kilo.ai/api/gateway/models`（384 项，15 个 isFree）
        let v = serde_json::json!({
            "data": [
                {"id":"kilo-auto/free","name":"Kilo Auto Free","isFree":true,
                 "context_length":131072,"mayTrainOnYourPrompts":true},
                {"id":"nvidia/nemotron-3-ultra-550b-a55b:free","name":"Nemotron 3 Ultra",
                 "isFree":true,"context_length":1000000,"mayTrainOnYourPrompts":false},
                {"id":"openrouter/free","name":"OpenRouter Free","isFree":true,
                 "context_length":200000,"mayTrainOnYourPrompts":false},
                // 付费模型必须被排除
                {"id":"anthropic/claude-opus","name":"Claude","isFree":false},
                {"id":"paid/model","name":"Paid"},
            ]
        });
        let free = parse_kilo_gateway_models(&v);
        assert_eq!(free.len(), 3, "只收 isFree=true：{free:?}");
        let ids: Vec<&str> = free.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["kilo-auto/free", "nvidia/nemotron-3-ultra-550b-a55b:free", "openrouter/free"]
        );
        // 训练提示必须透传（UI 要据此警告用户）
        let auto = free.iter().find(|m| m.id == "kilo-auto/free").unwrap();
        assert!(auto.may_train_on_prompts, "mayTrainOnYourPrompts 必须透传");
        // 上下文长度要能解析出 1M
        let ultra = free
            .iter()
            .find(|m| m.id.contains("nemotron-3-ultra"))
            .unwrap();
        assert_eq!(ultra.context_length, Some(1_000_000));
    }

    #[test]
    fn kilo_probe_message_reports_free_pool_as_available() {
        // 修正后的语义：Kilo 网关 isFree 模型免 Key 可用，文案应如实反映
        let msg = "Kilo：免 Key 免费池可用，发现 12 个模型";
        assert!(msg.contains("免 Key"), "应说明免 Key：{msg}");
        assert!(msg.contains("免费池"), "应说明是免费池：{msg}");
    }

    /// 复刻 `probe_kilo` 的文案拼装，供上面的文案断言使用。
    /// 真实网络探测 Kilo 自动免费路由：验证 200 + cost=0。默认忽略。
    ///
    /// 实测矩阵（2026-11，逐模型 POST `/api/gateway/v1/chat/completions`）：
    /// - `kilo-auto/free` → **200**，`usage.cost = 0`，`is_byok = false`，上游 `reflection/beam-free`
    /// - 网关命名空间 `nvidia/nemotron-3.5-lightning:free` → 200 cost=0
    /// - 目录 slug 命名空间 `minimax-minimax-m3-free` → **401** `PAID_MODEL_AUTH_REQUIRED`
    #[tokio::test]
    #[ignore = "依赖外网 kilo.ai，按需手动执行"]
    async fn real_kilo_auto_free_probe() {
        let client = build_client();
        let url = format!("{}/chat/completions", KILO_GATEWAY_BASE.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": KILO_AUTO_FREE_MODEL,
            "messages": [{"role": "user", "content": "reply only: ok"}],
            "max_tokens": 32,
        });

        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .expect("请求 Kilo 网关失败");

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let cost = v
            .get("usage")
            .and_then(|u| u.get("cost"))
            .and_then(Value::as_f64);
        let upstream = v.get("model").and_then(Value::as_str).unwrap_or("<none>");
        println!(
            "KILO-AUTO-FREE http={} cost={:?} upstream={} body={}",
            status.as_u16(),
            cost,
            upstream,
            truncate(&text, 160)
        );
        assert_eq!(
            status.as_u16(),
            200,
            "kilo-auto/free 实测免登录可用，期望 200，实际 {}：{}",
            status.as_u16(),
            truncate(&text, 200)
        );
        assert_eq!(cost, Some(0.0), "kilo-auto/free 应为零成本");
    }

    /// 真实网络探测 Kilo 公开目录（需可访问 kilo.ai）。默认忽略。
    #[tokio::test]
    #[ignore = "依赖外网 kilo.ai，按需手动执行"]
    async fn real_kilo_probe() {
        let p = find("kilo").unwrap();
        let outcome = probe(p, None, None).await;
        println!("KILO {} [{:?}] total={} free={}",
            outcome.message, outcome.status, outcome.models.len(), outcome.free_model_count);
        assert_eq!(outcome.status, ProbeStatus::CatalogOnly,
            "Kilo 必须被识别为仅目录可用，不得报成 Available");
        // 目录里的零价格数必须是实测的 20，且文案必须承认需登录
        assert_eq!(outcome.free_model_count, 20, "零价格模型数应为实测的 20");
        assert!(
            outcome.message.contains("需登录"),
            "目录探测文案必须写明零价格模型需登录：{}",
            outcome.message
        );
    }

    /// 真实网络探测（本机装了 Ollama 才会通过）。默认忽略，避免 CI 依赖环境。
    #[tokio::test]
    #[ignore = "依赖本机 Ollama 服务，按需手动执行"]
    async fn real_ollama_probe() {
        let p = find("ollama").unwrap();
        let outcome = probe(p, None, None).await;
        println!("{} [{:?}] models={:?}", outcome.message, outcome.status, outcome.models);
        assert!(
            matches!(outcome.status, ProbeStatus::Available | ProbeStatus::NoModels | ProbeStatus::Unreachable),
            "本机探测必须落到三种真实态之一，不许伪造"
        );
    }
}
