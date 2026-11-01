# 模型接入、省 Token 与命令审批 — 设计文档

> 本文是三项能力的**唯一实现契约**。参考开源项目 `Ebony-Vinyl/dsh-our-free-model`
> 的产品形态，但按其约束做了取舍（见 §1.2）。实现前先读本节，实现时不得偏离本契约。

---

## 1. 范围与边界

### 1.1 三项能力

| 能力 | 目标 | 验收 |
|---|---|---|
| **A. 模型接入扩展** | 聚合渠道（多提供商注册表）+ 本机模型 + 免 Key 免费池 | 选择器里能列出真实可用模型并按 provider 路由 |
| **B. 省 Token** | 工具输出压缩（RTK）+ 节省统计 | 大输出被压缩且模型仍能作答，统计可查 |
| **C. 命令审批** | 分级审批 + 高危二次确认 + 审计日志 | 高危命令必须二次输入确认，全量留痕 |

### 1.2 明确不做（与参考项目的差异）

参考项目的免费车道依赖 **OpenCode Zen 网关**，其接入要求「每请求伪造会话 id + 特定
User-Agent + 官方工具指纹」。这是对抗上游风控的行为，会给用户带来账号/IP 被封风险。
**本项目不实现**，也不提供伪造指纹的任何代码路径。

同理不实现：EAC 协付渠道（需 GitHub Star 门槛与宿主指纹）、十个账号渠道（需各厂商
专有登录）、插件热升级、本地 Anthropic 网关、降级链的 `round-robin` 粘滞策略。

### 1.3 保留的「白嫖」路径（真实可行）

| 渠道 | 是否需要 Key | 实现方式 | 实测状态 |
|---|---|---|---|
| **本地 Ollama** | 否 | `/api/tags` + `/v1/*`（OpenAI 兼容） | ✅ **已端到端验证**：本机 Ollama 0.32.9 + `qwen2.5:0.5b`，真实回复并返回 usage |
| **本地 LM Studio** | 否 | `http://127.0.0.1:1234/v1/models` | 未实测（本机未安装） |
| **本地 llama.cpp server** | 否 | `http://127.0.0.1:8080/v1/models` | 未实测（本机未安装） |
| **Kilo 模型目录** | **是**（需 Kilo 账号） | `GET https://kilo.ai/api/models` | ⚠️ 目录可达（158 个模型），但**推理端点 404**，不能当免费通道 |
| **聚合渠道（40+ 家）** | 是（部分本机豁免） | 用户自填 Key，`provider/model` 命名空间 | 按用户 Key 而定 |
| **免费层（free-tier）** | 是 | 同上，但标注免费额度 | 按用户 Key 而定 |

### 1.3.1 Kilo 的真实结构（实测，勿按参考项目假设）

参考项目文档称 Kilo 有「`isFree` 清单实时拉取，无需任何账号或 Key」。**实测不符**：

- `GET https://kilo.ai/api/models` 返回 158 个模型的目录，字段中**没有 `isFree`**；
  免费与否由价格字段表达：`"priceInput":"0.000000"`、`"priceOutput":"0.000000"`
  → 零价格的共 **20 个**（含 `minimax-minimax-m3-free`、`nvidia-nemotron-3-ultra-…-free`）。
- `POST https://kilo.ai/api/gateway/v1/chat/completions` 与 `POST https://kilo.ai/v1/chat/completions`
  均返回 **HTTP 404**（落到 Next.js 404 页）。

**结论**（经两轮独立复验，结论比初判更精确）：

Kilo 网关存在**两套互不相同的 model id 命名空间**，同一个模型的两个 id 行为相反：

| 命名空间 | 例 | POST 结果 |
|---|---|---|
| 目录 `/api/models`（连字符 + `-free`） | `nvidia-nemotron-3-ultra-550b-a55b-free` | **401** `PAID_MODEL_AUTH_REQUIRED` |
| 网关 `/api/gateway/v1/models`（斜杠 + `:free`） | `nvidia/nemotron-3.5-lightning:free`、`openrouter/free` | **200**，`cost: 0`，`is_byok: false` |

此外自动路由 `kilo-auto/free` 稳定返回 200 / `cost: 0`（上游 `reflection/beam-free`）。

**因此实现的正确姿势**：
- 目录（`/api/models`）只用于**展示与价格标注**，其零价格模型必须标为
  「标价 $0，但需登录 Kilo 账号才能调用」，**不得声称可直接调用**。
- 网关（`/api/gateway/v1/models`）的 `:free` 系列与 `kilo-auto/free` 是**确实免登录**
  的通道，可标注为免 Key 可用。
- 注册表 `kilo` 的 `category` 保持需要账号的语义（`ApiKey`/`FreeTier`），
  不为它单独造一个「免费池」概念。

这个差异是导致两轮实测得出相反结论的根因，已写进 `channels.rs` 的模块注释。

> 免责声明必须随 UI 一起出现：免费池由第三方提供，其模型卡可能声明 prompt 会被记录
> 用于改进服务；**不要发送敏感内容**。

---

## 1.4 账号渠道（已移除）

> **2026-11 决策：移除五个账号渠道（TRAE / Qoder / Qoder 中国版 / MiniMax / ZCode）。**
>
> 原因：
> 1. 它们依赖对厂商客户端的逆向（Qoder 需要官方 298KB WASM 加密请求体，
>    TRAE 需要逆向 Go 项目的 17 参数登录协议），属于绕过厂商对第三方客户端的使用限制。
> 2. 原计划要做的「账号池 / 验证码池」本质是多账号轮换规避免费额度限制，
>    违背厂商服务条款，有封号风险。
> 3. **Windows 上无法可靠地从 Tauri 应用内拉起外部浏览器**（已实测：
>    应用运行在未开启 `JOB_OBJECT_LIMIT_BREAKAWAY_OK` 的 Job Object 内，
>    `IsProcessInJob` 返回 True，`CREATE_BREAKAWAY_FROM_JOB` 被系统拒绝），
>    导致浏览器登录链路在目标平台上不可用。

## 2. 模块划分与写作用域

| 模块 | 路径 | 职责 |
|---|---|---|
| M-A1 注册表 | `src-tauri/src/llm/registry.rs` | 内置提供商目录、分类、默认 Base URL、认证方式 |
| M-A2 通道 | `src-tauri/src/llm/channels.rs` | 各通道的模型发现与可用性探测（本机 / Kilo / 聚合） |
| M-A3 路由 | `src-tauri/src/llm/router.rs` | `provider/model` 解析、降级链（fallback） |
| M-B1 压缩 | `src-tauri/src/token/rtk.rs` | 工具输出压缩器（纯函数 + 幂等） |
| M-B2 统计 | `src-tauri/src/token/stats.rs` | 压缩前后字节、节省比例累计 |
| M-C1 审批 | 扩 `src-tauri/src/guard/*` | 风险分级已有；新增审计与高危二次确认 |
| M-C2 审计 | `src-tauri/src/agent/audit.rs` | 审批与执行留痕 |
| M-F1 设置页 | `src/features/settings/**` | 渠道卡片、免费池、压缩统计 UI |

---

## 3. 数据契约

### 3.1 提供商注册表项

```rust
pub struct ProviderEntry {
    pub id: &'static str,           // 稳定标识，如 "openrouter"
    pub name: &'static str,         // 展示名
    pub category: ProviderCategory, // Free | FreeTier | ApiKey | OAuth | Local
    pub base_url: Option<&'static str>, // None = 必须由用户覆盖（Azure/Cloudflare/Vertex）
    pub auth: AuthKind,             // None | Bearer | Header(&str) ...
    pub docs_url: Option<&'static str>,
    pub note: Option<&'static str>, // 展示给用户的一句话说明
}

pub enum ProviderCategory { Free, FreeTier, ApiKey, OAuth, Local }
pub enum AuthKind { None, Bearer }
```

**分类语义**（UI 分组依据）：
- `Free`：无需任何 Key（Kilo 公有池、本机免账号）
- `FreeTier`：需 Key 但有免费额度
- `ApiKey`：需付费 Key
- `OAuth`：需交互式登录 → **只列出，标注「不可驱动」**
- `Local`：本机服务（Ollama / LM Studio / llama.cpp）

### 3.2 统一模型标识

```
<provider_id>/<model_id>
例：openrouter/anthropic-claude-3.5-sonnet
    ollama/qwen2.5:7b
    kilo/kilo-auto-free
    builtin/deepseek-chat          ← 用户直接填 base_url+key 的老路径保留
```

解析规则：**首个 `/` 之前为 provider_id**，其余整体为 model_id（model_id 自身可含 `/`，
如 OpenRouter 的 `anthropic/claude-3.5`）。若 provider_id 不在注册表内，
整个字符串视为 `builtin/` 下的模型。

### 3.3 配置补丁扩展（`AppConfigPatch.llm`）

```rust
pub struct LlmConfigPatch {
    // 既有字段保持
    pub provider: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub temperature: Option<f32>,
    pub clear_api_key: bool,

    // 新增
    pub active_model: Option<String>,        // 形如 "provider/model"
    pub channels: Option<Vec<ChannelPatch>>, // 渠道级增改
    pub combos: Option<Vec<ComboPatch>>,     // 降级链
}

pub struct ChannelPatch {
    pub provider_id: String,
    pub enabled: Option<bool>,
    pub base_url_override: Option<String>,
    pub api_key: Option<String>,
    pub clear_api_key: Option<bool>,
    pub model_overrides: Option<Vec<String>>, // 覆盖模型清单
}

pub struct ComboPatch {
    pub name: String,        // 以 combo:<name> 暴露
    pub strategy: String,    // 仅 "fallback"
    pub members: Vec<String>,
}
```

### 3.4 省 Token 统计

```rust
pub struct CompressionStats {
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub saved_bytes: u64,
    pub blocks_compressed: u64,
    pub calls: u64,
}
```

---

## 4. RTK 压缩算法契约

### 4.1 适用对象

只对 **`role:"tool"` 的工具输出**做压缩，天然不触碰：用户消息、assistant 正文、系统提示词。

### 4.2 安全规则（硬性，违反即视为 bug）

1. **报错的工具结果永不压缩** —— 结果以求知型错误标记开头或含 `error`/`failed`/`panic` 等
   且输出较短时，原样放行。宁可多花 token，不可隐藏报错。
2. **小于尺寸下限不压缩** —— 默认 `MIN_BYTES = 2048`。
3. **压缩结果永不大于原文** —— 若压缩后更大，回退原文。
4. **幂等** —— `compress(compress(x)) == compress(x)`。
5. **失败即放行** —— 任何过滤器 panic/异常时，返回原文。

### 4.3 过滤器（按优先级）

| 过滤器 | 识别特征 | 压缩策略 |
|---|---|---|
| unified diff | 含 `diff --git` / `@@ ... @@` | 保留 hunk 头与变更行，折叠未变更上下文；超长时只留前后各 N 行 |
| git status | `On branch` / `Changes not staged` | 统计各类文件数，折叠同类列表 |
| git log | 连续 `commit <sha>` 块 | 保留前 N 条完整，其余折叠为一行摘要 |
| 文件列表（ls/find/tree） | 大量短行、多为路径 | 保留前 N 行 + `… 其余 M 项` |
| grep 结果 | `path:line:content` 重复前缀 | 按文件聚合，同文件只留前 K 行 |
| 重复日志行 | 相邻完全相同行 | 折叠为 `重复 ×N` |

每个过滤器都必须能 **拒绝处理**（返回 `None`），交由下一个或直接放弃。

### 4.4 压缩标记

压缩后的文本**必须**在末尾追加可回查提示，例如：

```
[已压缩：原始 40.2 KB → 3.9 KB，省略 214 行。需要完整内容请执行 `git diff`]
```

这样模型知道自己看到的是压缩视图，可以主动要求原文 —— 这是「压缩不降低可答性」的关键。

---

## 5. 命令审批契约

### 5.1 三级风险（沿用既有分类器）

| 级别 | 处置 |
|---|---|
| `ReadOnly` | 自动执行 |
| `Mutating` | 单次确认（允许一次 / 拒绝） |
| `Dangerous` | **二次确认**：必须勾选「我已了解风险」或输入指定短语，才可放行 |

### 5.2 二次确认规则

- `Dangerous` 命令的审批卡片必须展示：完整命令、风险原因、影响范围提示
- 确认短语固定为 `确认执行`（精确匹配，不做模糊）
- 拒绝与超时（300s）一律不执行，并记录到审计

### 5.3 审计日志

每条审批产生一条记录，落盘到 `audit.jsonl`（追加写，线程安全）：

```rust
pub struct AuditEntry {
    pub ts: i64,
    pub session_id: String,
    pub command: String,
    pub risk: String,        // read_only / mutating / dangerous
    pub decision: String,    // auto / approved / rejected / timeout / aborted
    pub reason: Option<String>,
}
```

敏感命令**不做脱敏**（审计的目的就是可回溯），但只存本机。

---

## 6. 与既有代码的接入点

| 接入点 | 改动 |
|---|---|
`AgentRuntime::send` | 解析 `active_model` → 选 provider → 每次 tool 结果过 RTK |
`guard::classify` | 不变 |
审批等待 | 增加 `Dangerous` 分支的二次确认状态 |
`ipc::agent_approve` | 增加可选 `confirmPhrase` 参数 |
`SettingsPanel` | 新增「聚合渠道」「省 Token」两个分区 |

---

## 7. 验收清单

- [ ] `cargo check --release` 零 error
- [ ] `pnpm typecheck` 零 error
- [ ] RTK 单测：diff/status/log/ls/grep/重复行 各 1 例；幂等、回退、报错不压缩各 1 例
- [ ] 真实压缩验证：对真实仓库跑 `git diff` 并断言节省比例
- [ ] 路由单测：`provider/model` 解析、未知 provider 回退 builtin
- [ ] 高危命令二次确认：短语不匹配则不放行
- [ ] 审计落盘并可读回
- [ ] 本机 Ollama 探测（若本机无 Ollama，如实提示「未安装」而非伪造）
