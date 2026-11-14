//! IPC DTO —— 与 `src/shared/ipc.ts` 严格一一对应。
//!
//! 变更本文件必须同步修改前端类型与 docs/ARCHITECTURE.md §5。

use serde::{Deserialize, Serialize};

// ───────────────────────────── 基础 ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Connecting,
    Auth,
    Connected,
    Closed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardState {
    Starting,
    Running,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    ReadOnly,
    Mutating,
    Dangerous,
}

// ───────────────────────────── 配置 ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmConfig {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    /// 前端永远拿不到明文 key，只知道是否已配置
    pub has_api_key: bool,
    pub temperature: f32,
    /// 当前生效的模型 id，形如 `provider/model`（空 = 用 builtin 通道）
    #[serde(default)]
    pub active_model: String,
    /// 各渠道的用户配置（Key 业务侧永不回显，只回 has_key）
    #[serde(default)]
    pub channels: Vec<ChannelConfig>,
    /// 命名降级链
    #[serde(default)]
    pub combos: Vec<ComboConfig>,
}

/// 单个渠道的用户侧配置（**不序列化明文 Key**，只暴露 has_key）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelConfig {
    pub provider_id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url_override: Option<String>,
    /// 仅后端使用：明文 Key。对外序列化时用 `has_key` 代替，见 `public_config`。
    #[serde(default, skip_serializing)]
    pub api_key: Option<String>,
    #[serde(default = "default_true")]
    pub has_key: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_overrides: Option<Vec<String>>,
}

/// 命名降级链配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComboConfig {
    pub name: String,
    #[serde(default = "default_combo_strategy")]
    pub strategy: String,
    #[serde(default)]
    pub members: Vec<String>,
}

fn default_combo_strategy() -> String {
    "fallback".into()
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "openai_compat".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            model: String::new(),
            has_api_key: false,
            temperature: 0.2,
            active_model: String::new(),
            channels: Vec::new(),
            combos: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalConfig {
    pub font_size: u32,
    pub scrollback: u32,
    /// 终端字体族覆盖；空字符串表示使用内置字体栈。
    #[serde(default)]
    pub font_family: String,
    /// 光标样式：block / bar / underline
    #[serde(default = "default_cursor_style")]
    pub cursor_style: String,
    #[serde(default = "default_true")]
    pub cursor_blink: bool,
}

fn default_cursor_style() -> String {
    "block".into()
}

fn default_true() -> bool {
    true
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            font_size: 14,
            scrollback: 5000,
            font_family: String::new(),
            cursor_style: "block".into(),
            cursor_blink: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiConfig {
    pub theme: String,
    /// `"system"`（默认，跟随系统）/ `"zh"` / `"en"`。
    ///
    /// 用三态而不是两态：`"system"` 让「默认跟随系统语言」与
    /// 「用户显式选择了中文」可区分 —— 否则后端默认写 `"zh"`，
    /// 前端无法知道你到底是没设置过、还是就想要中文。
    #[serde(default = "default_language")]
    pub language: String,
    pub sidebar_width: u32,
    pub agent_panel_width: u32,
    pub show_agent_panel: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            language: default_language(),
            sidebar_width: 240,
            agent_panel_width: 384,
            show_agent_panel: true,
        }
    }
}

fn default_language() -> String {
    "system".to_string()
}

/// 行为类开关。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorConfig {
    pub confirm_on_close_tab: bool,
    pub bell_on_disconnect: bool,
    pub auto_reconnect_attempts: u32,
    pub default_port: u16,
    pub default_username: String,
    /// 主机密钥校验策略：`strict`（默认）/ `accept_new` / `insecure`
    #[serde(default = "default_host_key_policy")]
    pub host_key_policy: String,
    /// 自定义 known_hosts 路径；空串 = `~/.ssh/known_hosts`
    #[serde(default)]
    pub known_hosts_path: String,
}

fn default_host_key_policy() -> String {
    "strict".into()
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self {
            confirm_on_close_tab: true,
            bell_on_disconnect: true,
            auto_reconnect_attempts: 0,
            default_port: 22,
            default_username: String::new(),
            host_key_policy: default_host_key_policy(),
            known_hosts_path: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub llm: LlmConfig,
    pub terminal: TerminalConfig,
    pub ui: UiConfig,
    pub behavior: BehaviorConfig,
}

/// 写入配置时的补丁结构；`apiKey` 只写不读。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfigPatch {
    pub llm: Option<LlmConfigPatch>,
    pub terminal: Option<TerminalConfigPatch>,
    pub ui: Option<UiConfigPatch>,
    pub behavior: Option<BehaviorConfigPatch>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmConfigPatch {
    pub provider: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub temperature: Option<f32>,
    /// 显式清空已保存的 key
    #[serde(default)]
    pub clear_api_key: bool,
    /// 切换当前生效模型（形如 `provider/model`）
    pub active_model: Option<String>,
    /// 渠道级增改（按 provider_id 匹配）
    #[serde(default)]
    pub channels: Option<Vec<ChannelPatch>>,
    /// 降级链全量替换
    #[serde(default)]
    pub combos: Option<Vec<ComboConfig>>,
}

/// 渠道补丁：只改传入的字段。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPatch {
    pub provider_id: String,
    pub enabled: Option<bool>,
    pub base_url_override: Option<String>,
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_api_key: bool,
    pub model_overrides: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalConfigPatch {
    pub font_size: Option<u32>,
    pub scrollback: Option<u32>,
    pub font_family: Option<String>,
    pub cursor_style: Option<String>,
    pub cursor_blink: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiConfigPatch {
    pub theme: Option<String>,
    pub language: Option<String>,
    pub sidebar_width: Option<u32>,
    pub agent_panel_width: Option<u32>,
    pub show_agent_panel: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorConfigPatch {
    pub confirm_on_close_tab: Option<bool>,
    pub bell_on_disconnect: Option<bool>,
    pub auto_reconnect_attempts: Option<u32>,
    pub default_port: Option<u16>,
    pub default_username: Option<String>,
    pub host_key_policy: Option<String>,
    pub known_hosts_path: Option<String>,
}

// ───────────────────────────── 主机 ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostProfile {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
    pub updated_at: i64,
}

// ───────────────────────────── SSH ─────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthSpec {
    Password { password: String },
    Key { key_path: String, passphrase: Option<String> },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectRequest {
    pub host_id: Option<String>,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthSpec,
    pub cols: u32,
    pub rows: u32,
    /// 用户在指纹确认弹层里点「信任」后带上的 OpenSSH 公钥。
    ///
    /// 只有「未知主机」会用到它：命中则写入 known_hosts 并放行；
    /// 与 `known_hosts` 中已有密钥冲突时**永远拒绝**（需要人工处理）。
    #[serde(default)]
    pub trust_host_key: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub state: SessionState,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_key_fingerprint: Option<String>,
    /// 主机密钥校验结果：`verified` / `trusted` / `learned` / `insecure`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_key_status: Option<String>,
}

/// `ssh_probe_host_key` 的返回：只握手不认证，用于「未知主机」的指纹确认。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyProbe {
    pub host: String,
    pub port: u16,
    /// `SHA256:...`
    pub fingerprint: String,
    /// `ssh-ed25519` 等
    pub algorithm: String,
    /// 完整公钥（前端原样回传给 `ssh_connect` 的 `trustHostKey`）
    pub public_key: String,
    pub known_hosts_path: String,
    /// 当前生效策略
    pub policy: String,
    /// 与 known_hosts 的比对结果：`match` / `unknown` / `mismatch` / `revoked`
    pub status: String,
}

// ───────────────────────────── 端口转发 ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardKind {
    Local,
    Remote,
    Dynamic,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardRequest {
    pub session_id: String,
    pub kind: ForwardKind,
    pub listen_host: String,
    pub listen_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

/// 修改转发规则时的补丁：只改传入的字段。
///
/// 运行中的规则被修改时会**原地重启**（保留 `id`，先释放旧监听 / 取消旧的
/// `tcpip-forward`，再按新参数重新启动）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardPatch {
    pub session_id: Option<String>,
    pub kind: Option<ForwardKind>,
    pub listen_host: Option<String>,
    pub listen_port: Option<u16>,
    pub remote_host: Option<String>,
    pub remote_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardInfo {
    pub id: String,
    pub session_id: String,
    pub kind: ForwardKind,
    pub listen_host: String,
    pub listen_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    pub state: ForwardState,
    /// 规则创建时间（毫秒），用于稳定排序
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ───────────────────────────── Agent ─────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSendRequest {
    pub run_id: String,
    pub session_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentApproveRequest {
    pub approval_id: String,
    pub approved: bool,
    /// 高危（`Dangerous`）命令的二次确认短语，必须精确等于 `确认执行`。
    ///
    /// `#[serde(default)]` 保证：不带该字段的旧前端调用不会反序列化失败，
    /// 但会因缺少短语而被拒绝 —— 闸门默认关闭，这是有意的 fail-safe。
    #[serde(default)]
    pub confirm_phrase: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
}

// ───────────────────────────── 渠道 / 省 Token / 审计 ─────────────────────────────

/// 给前端看的一家提供商（注册表 + 用户配置合并后的结果）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDto {
    pub id: String,
    pub name: String,
    /// local / free / free_tier / api_key
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// 该渠道是否需要 Key（本机通道为 false）
    pub requires_key: bool,
    /// 用户是否填过 Key（永不返回明文）
    pub has_key: bool,
    pub enabled: bool,
}

/// 探测结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResultDto {
    pub provider_id: String,
    /// available / no_models / unreachable / unauthorized / catalog_only / unsupported
    pub status: String,
    pub message: String,
    pub models: Vec<String>,
    pub free_model_count: usize,
}

/// 压缩节省统计。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressionStatsDto {
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub saved_bytes: u64,
    pub blocks_compressed: u64,
    pub calls: u64,
    /// 0.0 – 1.0
    pub saved_ratio: f64,
}

/// 一条审批审计记录。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntryDto {
    pub ts: i64,
    pub session_id: String,
    pub command: String,
    pub risk: String,
    /// auto / approved / rejected / timeout / aborted
    pub decision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
