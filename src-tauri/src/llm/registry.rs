//! 提供商注册表（M-A1）。
//!
//! 契约见 `docs/FEATURE-MODEL-PROVIDERS.md` §3.1。
//!
//! **重要边界（§1.2）**：本注册表只登记「合规可达」的通道。
//! 不包含、也不提供任何需要伪造会话 id / User-Agent / 官方工具指纹才能接通的网关
//! （即参考项目里的 OpenCode Zen 路径）。任何伪造指纹的代码路径都不存在于本模块。
//!
//! URL 采集原则：**data 里的 base_url / docs_url 都是本轮实测探过的**。
//! 端点不可达或形态不确定的提供商，`base_url` 一律填 `None`，并在 `note` 里写明
//! 「需手工覆盖」，**不编造 URL**。

use serde::{Deserialize, Serialize};

/// 提供商分类（UI 分组依据，语义见契约 §3.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCategory {
    /// 无需任何 Key（本机 Ollama / LM Studio / llama.cpp）。
    Local,
    /// 无需任何 Key 即可调用的公共池。
    ///
    /// Kilo 属于此类：其网关 `isFree: true` 的模型实测免 Key、`cost: 0`
    /// （见 `channels` 模块的 Kilo 注释与 `kilo_free_models` 常量）。
    Free,
    /// 需 Key 但有免费额度。
    FreeTier,
    /// 需付费 Key。
    ApiKey,
    /// 需交互式登录 → 只列出，标注「不可驱动」。
    OAuth,
}

impl ProviderCategory {
    /// 稳定的字符串标识，供 IPC / 前端分组使用（不要用 Debug 输出，它可能变）。
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderCategory::Local => "local",
            ProviderCategory::Free => "free",
            ProviderCategory::FreeTier => "free_tier",
            ProviderCategory::ApiKey => "api_key",
            ProviderCategory::OAuth => "oauth",
        }
    }

    /// 展示顺序：本机 → 免费 → 免费层 → Key → OAuth。
    pub fn rank(self) -> u8 {
        match self {
            ProviderCategory::Local => 0,
            ProviderCategory::Free => 1,
            ProviderCategory::FreeTier => 2,
            ProviderCategory::ApiKey => 3,
            ProviderCategory::OAuth => 4,
        }
    }

    /// 中文展示名。
    pub fn label(self) -> &'static str {
        match self {
            ProviderCategory::Local => "本机模型",
            ProviderCategory::Free => "免费通道",
            ProviderCategory::FreeTier => "免费额度",
            ProviderCategory::ApiKey => "API Key",
            ProviderCategory::OAuth => "OAuth 登录",
        }
    }

    /// 该分类是否可被本应用直接驱动（发起对话）。
    /// OAuth 类只列出、不驱动（契约 §3.1）。
    pub fn is_drivable(self) -> bool {
        !matches!(self, ProviderCategory::OAuth)
    }
}

/// 认证方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    /// 无需认证（本机服务）。
    None,
    /// `Authorization: Bearer <key>`。
    Bearer,
}

/// 注册表项（契约 §3.1 原样字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEntry {
    /// 稳定标识，如 `"openrouter"`；`provider/model` 命名空间的第一段。
    pub id: &'static str,
    /// 展示名。
    pub name: &'static str,
    pub category: ProviderCategory,
    /// `None` = 必须由用户覆盖（Azure / Cloudflare / Vertex 等租户相关端点）。
    pub base_url: Option<&'static str>,
    pub auth: AuthKind,
    pub docs_url: Option<&'static str>,
    /// 展示给用户的一句话说明。
    pub note: Option<&'static str>,
}

impl ProviderEntry {
    /// `provider/model` 形式的完整模型标识。
    pub fn qualify(&self, model: &str) -> String {
        format!("{}/{}", self.id, model)
    }

    /// 该提供商是否需要用户填 Key。
    pub fn needs_key(&self) -> bool {
        matches!(self.auth, AuthKind::Bearer)
    }
}

/// 本机模型的默认端点。
pub const OLLAMA_DEFAULT_BASE: &str = "http://127.0.0.1:11434/v1";
pub const LMSTUDIO_DEFAULT_BASE: &str = "http://127.0.0.1:1234/v1";
pub const LLAMACPP_DEFAULT_BASE: &str = "http://127.0.0.1:8080/v1";

/// Kilo 网关的**正确**推理入口（OpenAI 兼容）。
///
/// ⚠️ 主机是 `api.kilo.ai`，不是 `kilo.ai`。这是实测确认的关键差异：
/// `kilo.ai/api/gateway/v1/...` 与 `kilo.ai/v1/...` 都会落到 Next.js 的 404 页，
/// 只有 `api.kilo.ai/api/gateway/...` 才是真网关。
pub const KILO_GATEWAY_BASE: &str = "https://api.kilo.ai/api/gateway";

/// Kilo 网关的模型清单（含 `isFree` 字段，是**推理侧**的权威清单）。
pub const KILO_GATEWAY_MODELS_URL: &str = "https://api.kilo.ai/api/gateway/models";

/// Kilo 官网的模型目录（含 `priceInput`/`priceOutput`，**仅供展示与比价**）。
/// 注意这份目录里的 `xxx-free` 连字符 id 与网关的 `xxx:free` 是不同命名空间，
/// 前者大多需要登录，不要用它来判断「是否可免费调用」。
pub const KILO_CATALOG_URL: &str = "https://kilo.ai/api/models";

/// 内置提供商目录。
///
/// 顺序即 UI 默认展示顺序，按 `category.rank()` 分组后再按此处的登记顺序。
pub const PROVIDERS: &[ProviderEntry] = &[
    // ───────────────────────── Local（无需 Key，真实可驱动）─────────────────────────
    ProviderEntry {
        id: "ollama",
        name: "Ollama",
        category: ProviderCategory::Local,
        base_url: Some(OLLAMA_DEFAULT_BASE),
        auth: AuthKind::None,
        docs_url: Some("https://docs.ollama.com"),
        note: Some("本机 Ollama；默认 127.0.0.1:11434，先 `ollama serve` 再拉模型。零成本、无 Key。"),
    },
    ProviderEntry {
        id: "lmstudio",
        name: "LM Studio",
        category: ProviderCategory::Local,
        base_url: Some(LMSTUDIO_DEFAULT_BASE),
        auth: AuthKind::None,
        docs_url: Some("https://lmstudio.ai/docs"),
        note: Some("本机 LM Studio；需在 LM Studio 里开启本地服务器（默认 1234 端口）。"),
    },
    ProviderEntry {
        id: "llamacpp",
        name: "llama.cpp server",
        category: ProviderCategory::Local,
        base_url: Some(LLAMACPP_DEFAULT_BASE),
        auth: AuthKind::None,
        docs_url: Some("https://github.com/ggml-org/llama.cpp"),
        note: Some("本机 llama.cpp server；用 `llama-server` 启动，默认 8080 端口。"),
    },
    // ───────────────────────── FreeTier（需 Key，但有免费额度）─────────────────────────
    ProviderEntry {
        id: "openrouter",
        name: "OpenRouter",
        category: ProviderCategory::FreeTier,
        base_url: Some("https://openrouter.ai/api/v1"),
        auth: AuthKind::Bearer,
        docs_url: Some("https://openrouter.ai/docs"),
        note: Some("聚合 300+ 模型；`:free` 后缀模型有免费额度，需注册获取 Key。"),
    },
    ProviderEntry {
        id: "groq",
        name: "Groq",
        category: ProviderCategory::FreeTier,
        base_url: Some("https://api.groq.com/openai/v1"),
        auth: AuthKind::Bearer,
        docs_url: Some("https://console.groq.com/docs"),
        note: Some("超快 LPU 推理；有免费额度与速率限制，需注册 Key。"),
    },
    ProviderEntry {
        id: "cerebras",
        name: "Cerebras",
        category: ProviderCategory::FreeTier,
        base_url: Some("https://api.cerebras.ai/v1"),
        auth: AuthKind::Bearer,
        docs_url: Some("https://inference-docs.cerebras.ai"),
        note: Some("wafer-scale 超快推理；有每日免费 token 额度。"),
    },
    ProviderEntry {
        id: "sambanova",
        name: "SambaNova",
        category: ProviderCategory::FreeTier,
        base_url: None,
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("有免费层；本机实测其 API 域名不可达（TLS 失败），base_url 需手工覆盖。"),
    },
    ProviderEntry {
        id: "nvidia-nim",
        name: "NVIDIA NIM",
        category: ProviderCategory::FreeTier,
        base_url: Some("https://integrate.api.nvidia.com/v1"),
        auth: AuthKind::Bearer,
        docs_url: Some("https://docs.api.nvidia.com"),
        note: Some("NVIDIA 官方推理微服务；有免费开发额度，需 NGC Key。"),
    },
    ProviderEntry {
        id: "siliconflow",
        name: "SiliconFlow 硅基流动",
        category: ProviderCategory::FreeTier,
        base_url: Some("https://api.siliconflow.cn/v1"),
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("国内聚合；注册赠送额度，文档站本机不可达故未登记 docs_url。"),
    },
    ProviderEntry {
        id: "chutes",
        name: "Chutes",
        category: ProviderCategory::FreeTier,
        base_url: None,
        auth: AuthKind::Bearer,
        docs_url: Some("https://docs.chutes.ai"),
        note: Some("去中心化推理池，有免费档；本机实测 llm.chutes.ai 不可达，base_url 需手工覆盖。"),
    },
    ProviderEntry {
        id: "huggingface",
        name: "Hugging Face",
        category: ProviderCategory::FreeTier,
        base_url: None,
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("Inference Providers 有免费额度；路由端点本机不可达，base_url 需手工覆盖。"),
    },
    ProviderEntry {
        id: "cloudflare",
        name: "Cloudflare Workers AI",
        category: ProviderCategory::FreeTier,
        base_url: None,
        auth: AuthKind::Bearer,
        docs_url: Some("https://developers.cloudflare.com"),
        note: Some("有免费每日额度；端点含 account id，必须手工覆盖 base_url 并填 account 段。"),
    },
    ProviderEntry {
        id: "z-ai",
        name: "Z.ai (GLM)",
        category: ProviderCategory::FreeTier,
        base_url: Some("https://api.z.ai/api/paas/v4"),
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("智谱 GLM 开放平台；有免费模型档位，需注册 Key。"),
    },
    ProviderEntry {
        id: "glm-coding",
        name: "Z.ai Coding Plan",
        category: ProviderCategory::FreeTier,
        base_url: Some("https://api.z.ai/api/coding/paas/v4"),
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("GLM 编码套餐专用端点；套餐内含额度，需用套餐 Key。"),
    },
    ProviderEntry {
        id: "chatanywhere",
        name: "ChatAnywhere",
        category: ProviderCategory::FreeTier,
        base_url: Some("https://api.chatanywhere.tech/v1"),
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("第三方转发，有免费试用额度；可用性依赖第三方，非官方渠道。"),
    },
    ProviderEntry {
        id: "cohere",
        name: "Cohere",
        category: ProviderCategory::FreeTier,
        base_url: Some("https://api.cohere.ai/compatible/v1"),
        auth: AuthKind::Bearer,
        docs_url: Some("https://docs.cohere.com"),
        note: Some("提供 OpenAI 兼容端点；有 trial Key 免费额度。"),
    },
    ProviderEntry {
        id: "ai21",
        name: "AI21 Labs",
        category: ProviderCategory::FreeTier,
        base_url: Some("https://api.ai21.com/studio/v1"),
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("Jamba 系列；有免费试用额度。"),
    },
    ProviderEntry {
        id: "upstage",
        name: "Upstage Solar",
        category: ProviderCategory::FreeTier,
        base_url: Some("https://api.upstage.ai/v1"),
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("Solar 系列韩系模型；有免费额度。"),
    },
    // ───────────────────────── ApiKey（需付费 Key）─────────────────────────
    ProviderEntry {
        id: "deepseek",
        name: "DeepSeek",
        category: ProviderCategory::ApiKey,
        base_url: Some("https://api.deepseek.com/v1"),
        auth: AuthKind::Bearer,
        docs_url: Some("https://api-docs.deepseek.com"),
        note: Some("本项目默认提供商；OpenAI 兼容，价格低。"),
    },
    ProviderEntry {
        id: "openai",
        name: "OpenAI",
        category: ProviderCategory::ApiKey,
        base_url: Some("https://api.openai.com/v1"),
        auth: AuthKind::Bearer,
        docs_url: Some("https://platform.openai.com/docs"),
        note: Some("官方 API；按量计费。"),
    },
    ProviderEntry {
        id: "moonshot",
        name: "Moonshot Kimi",
        category: ProviderCategory::ApiKey,
        base_url: Some("https://api.moonshot.cn/v1"),
        auth: AuthKind::Bearer,
        docs_url: Some("https://platform.moonshot.ai/docs"),
        note: Some("月之暗面 Kimi；OpenAI 兼容。"),
    },
    ProviderEntry {
        id: "minimax",
        name: "MiniMax",
        category: ProviderCategory::ApiKey,
        base_url: Some("https://api.minimax.chat/v1"),
        auth: AuthKind::Bearer,
        docs_url: Some("https://platform.minimaxi.com"),
        note: Some("MiniMax 开放平台；OpenAI 兼容端点。"),
    },
    ProviderEntry {
        id: "qwen",
        name: "Qwen 通义千问",
        category: ProviderCategory::ApiKey,
        base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1"),
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("阿里云 DashScope 兼容模式；需 DashScope Key。"),
    },
    ProviderEntry {
        id: "mistral",
        name: "Mistral AI",
        category: ProviderCategory::ApiKey,
        base_url: None,
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("官方 API 端点本机不可达（TLS 失败），base_url 需手工覆盖。"),
    },
    ProviderEntry {
        id: "together",
        name: "Together AI",
        category: ProviderCategory::ApiKey,
        base_url: Some("https://api.together.xyz/v1"),
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("开源模型托管；按 token 计费。"),
    },
    ProviderEntry {
        id: "fireworks",
        name: "Fireworks AI",
        category: ProviderCategory::ApiKey,
        base_url: Some("https://api.fireworks.ai/inference/v1"),
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("高速开源模型推理；按 token 计费。"),
    },
    ProviderEntry {
        id: "nebius",
        name: "Nebius AI Studio",
        category: ProviderCategory::ApiKey,
        base_url: None,
        auth: AuthKind::Bearer,
        docs_url: Some("https://docs.nebius.com"),
        note: Some("推理端点本机不可达，base_url 需手工覆盖。"),
    },
    ProviderEntry {
        id: "deepinfra",
        name: "DeepInfra",
        category: ProviderCategory::ApiKey,
        base_url: Some("https://api.deepinfra.com/v1/openai"),
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("低价开源模型推理；OpenAI 兼容端点。"),
    },
    ProviderEntry {
        id: "xai",
        name: "xAI Grok",
        category: ProviderCategory::ApiKey,
        base_url: None,
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("Grok 模型；端点本机不可达，base_url 需手工覆盖。"),
    },
    ProviderEntry {
        id: "perplexity",
        name: "Perplexity",
        category: ProviderCategory::ApiKey,
        base_url: None,
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("联网搜索型模型；端点本机不可达，base_url 需手工覆盖。"),
    },
    ProviderEntry {
        id: "azure-openai",
        name: "Azure OpenAI",
        category: ProviderCategory::ApiKey,
        base_url: None,
        auth: AuthKind::Bearer,
        docs_url: Some("https://learn.microsoft.com/azure"),
        note: Some("端点含 resource 名与 api-version，必须手工覆盖 base_url。"),
    },
    ProviderEntry {
        id: "vertex",
        name: "Google Vertex AI",
        category: ProviderCategory::ApiKey,
        base_url: None,
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("需 GCP 项目与区域构成端点，必须手工覆盖 base_url。"),
    },
    ProviderEntry {
        id: "bedrock",
        name: "AWS Bedrock",
        category: ProviderCategory::ApiKey,
        base_url: None,
        auth: AuthKind::Bearer,
        docs_url: Some("https://docs.aws.amazon.com/bedrock"),
        note: Some("AWS 签名认证（非 Bearer），本应用暂不支持其原生签名；需手工覆盖并自备签名代理。"),
    },
    ProviderEntry {
        id: "vercel",
        name: "Vercel AI Gateway",
        category: ProviderCategory::ApiKey,
        base_url: Some("https://ai-gateway.vercel.sh/v1"),
        auth: AuthKind::Bearer,
        docs_url: Some("https://ai-gateway.vercel.sh"),
        note: Some("Vercel 聚合网关；用 Vercel AI Gateway Key。"),
    },
    ProviderEntry {
        id: "anthropic",
        name: "Anthropic Claude",
        category: ProviderCategory::ApiKey,
        base_url: Some("https://api.anthropic.com/v1"),
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("原生 Messages API 与 OpenAI 格式不兼容；如需使用请走兼容代理并手工覆盖 base_url。"),
    },
    ProviderEntry {
        id: "jina",
        name: "Jina AI",
        category: ProviderCategory::ApiKey,
        base_url: None,
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("以 embedding / rerank 为主，端点本机不可达；chat 兼容性待验证，需手工覆盖。"),
    },
    ProviderEntry {
        id: "voyageai",
        name: "Voyage AI",
        category: ProviderCategory::ApiKey,
        base_url: None,
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("embedding 专用，不提供 chat completions；仅列出备查。"),
    },
    ProviderEntry {
        id: "informatics",
        name: "302.AI",
        category: ProviderCategory::ApiKey,
        base_url: None,
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("第三方聚合；端点本机不可达，base_url 需手工覆盖。"),
    },
    // ───────────────────────── OAuth（只列出，不可驱动）─────────────────────────
    ProviderEntry {
        id: "gemini",
        name: "Google Gemini",
        category: ProviderCategory::OAuth,
        base_url: Some("https://generativelanguage.googleapis.com/v1beta/openai"),
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("需 Google 账号交互式登录（OAuth），本应用只列出不驱动；可用 API Key 走兼容端点。"),
    },
    ProviderEntry {
        id: "google-adc",
        name: "Google ADC",
        category: ProviderCategory::OAuth,
        base_url: None,
        auth: AuthKind::Bearer,
        docs_url: None,
        note: Some("Application Default Credentials，依赖本机 gcloud 登录态，只列出不驱动。"),
    },
    // ──────────────── Kilo（免 Key 免费池，真实可用）────────────────
    ProviderEntry {
        id: "kilo",
        name: "Kilo",
        category: ProviderCategory::Free,
        base_url: Some(KILO_GATEWAY_BASE),
        auth: AuthKind::None,
        docs_url: Some("https://kilo.ai/docs"),
        note: Some(
            "免 Key 免费池：网关 `isFree` 清单里的模型实测可直接调用，`cost: 0`。\
             无需账号、无需注册。**上游声明 prompt 可能被记录用于改进服务，请勿发送敏感内容。**",
        ),
    },
];

/// 迁移/兼容用的伪提供商：用户直接填 base_url + key 的老路径（契约 §3.2）。
/// 不进入 [`PROVIDERS`]（它不是真实厂商，不出现在渠道选择器里），
/// 但 `parse_model_id` 必须认得它。
pub const BUILTIN_ID: &str = "builtin";

/// 全部提供商（按登记顺序）。
pub fn all() -> &'static [ProviderEntry] {
    PROVIDERS
}

/// 按 id 精确查找。
pub fn find(id: &str) -> Option<&'static ProviderEntry> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// 按分类筛选（保持登记顺序）。
pub fn by_category(cat: ProviderCategory) -> Vec<&'static ProviderEntry> {
    PROVIDERS.iter().filter(|p| p.category == cat).collect()
}

/// 按分类分组，按 `rank()` 排序，供 UI 直接渲染。
pub fn grouped() -> Vec<(ProviderCategory, Vec<&'static ProviderEntry>)> {
    let mut cats = [
        ProviderCategory::Local,
        ProviderCategory::Free,
        ProviderCategory::FreeTier,
        ProviderCategory::ApiKey,
        ProviderCategory::OAuth,
    ];
    cats.sort_by_key(|c| c.rank());
    cats.into_iter()
        .map(|c| (c, by_category(c)))
        .filter(|(_, list)| !list.is_empty())
        .collect()
}

/// id 是否在注册表内（`builtin` 视为已知）。
pub fn is_known_id(id: &str) -> bool {
    id == BUILTIN_ID || find(id).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn registry_has_at_least_forty_providers() {
        assert!(
            PROVIDERS.len() >= 40,
            "注册表至少 40 家，实际 {}",
            PROVIDERS.len()
        );
    }

    #[test]
    fn registry_ids_are_unique_and_non_empty() {
        let mut seen = HashSet::new();
        for p in PROVIDERS {
            assert!(!p.id.trim().is_empty(), "存在空 id");
            assert!(
                seen.insert(p.id),
                "id 重复: {}（provider/model 命名空间必须唯一）",
                p.id
            );
        }
    }

    #[test]
    fn registry_entries_are_complete() {
        for p in PROVIDERS {
            assert!(!p.name.trim().is_empty(), "{} 缺 name", p.id);
            assert!(
                p.note.is_some(),
                "{} 缺 note（UI 需要一句话中文说明）",
                p.id
            );
            // base_url 为 Some 时必须是完整 http(s) URL，且不带尾斜杠
            if let Some(u) = p.base_url {
                assert!(
                    u.starts_with("http://") || u.starts_with("https://"),
                    "{} 的 base_url 不是完整 URL: {u}",
                    p.id
                );
                assert!(
                    !u.ends_with('/'),
                    "{} 的 base_url 不应带尾斜杠: {u}",
                    p.id
                );
            } else {
                // 契约：None 必须在 note 里说明原因
                let note = p.note.unwrap_or("");
                assert!(
                    note.contains("手工覆盖")
                        || note.contains("仅列出")
                        || note.contains("不支持")
                        || note.contains("不驱动"),
                    "{} 的 base_url 为 None 时 note 必须说明原因，实际: {note}",
                    p.id
                );
            }
            // 本机通道必须无认证且有默认端点
            if p.category == ProviderCategory::Local {
                assert_eq!(p.auth, AuthKind::None, "本机通道 {} 不应需要 Key", p.id);
                assert!(p.base_url.is_some(), "本机通道 {} 必须有默认 base_url", p.id);
            }
        }
    }

    #[test]
    fn local_channels_are_the_three_expected() {
        let local: Vec<_> = by_category(ProviderCategory::Local)
            .iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(local, vec!["ollama", "lmstudio", "llamacpp"]);
    }

    #[test]
    fn category_distribution_is_reasonable() {
        // 每个分类都要有实际内容，且 Local 不能被归到别处
        for cat in [
            ProviderCategory::Local,
            ProviderCategory::Free, // Kilo 免费池
            ProviderCategory::FreeTier,
            ProviderCategory::ApiKey,
            ProviderCategory::OAuth,
        ] {
            assert!(
                !by_category(cat).is_empty(),
                "分类 {:?} 为空",
                cat
            );
        }
        // Free 现在有 Kilo：其网关 isFree 模型实测免 Key、cost=0（见 channels 模块注释）
        let free = by_category(ProviderCategory::Free);
        assert_eq!(free.len(), 1, "Free 分类应恰好是 Kilo 免费池");
        assert_eq!(free[0].id, "kilo");
        assert_eq!(free[0].auth, AuthKind::None, "免 Key 通道不得要求认证");
    }

    #[test]
    fn find_and_grouped_work() {
        assert_eq!(find("ollama").map(|p| p.category), Some(ProviderCategory::Local));
        assert!(find("nonexistent-provider").is_none());
        assert!(is_known_id("builtin"));
        assert!(is_known_id("ollama"));
        assert!(!is_known_id("nope"));

        let groups = grouped();
        let total: usize = groups.iter().map(|(_, v)| v.len()).sum();
        assert_eq!(total, PROVIDERS.len(), "分组不应丢项");
        // 首组必须是最低 rank
        assert_eq!(groups[0].0, ProviderCategory::Local);
    }

    #[test]
    fn oauth_category_is_not_drivable() {
        assert!(!ProviderCategory::OAuth.is_drivable());
        assert!(ProviderCategory::Local.is_drivable());
        for p in by_category(ProviderCategory::OAuth) {
            assert!(
                p.note.unwrap_or("").contains("不驱动") || p.note.unwrap_or("").contains("列出"),
                "{} 是 OAuth 类，note 必须标注不可驱动",
                p.id
            );
        }
    }

    #[test]
    fn qualify_builds_namespaced_id() {
        let p = find("ollama").unwrap();
        assert_eq!(p.qualify("qwen2.5:7b"), "ollama/qwen2.5:7b");
        assert!(p.needs_key() == false);
        assert!(find("openrouter").unwrap().needs_key());
    }
}
