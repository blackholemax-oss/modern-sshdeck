//! 本地持久化：配置、主机档案。
//!
//! 存储位置：
//! - Windows: `%APPDATA%\modern-sshdeck\`
//! - Linux:   `~/.config/modern-sshdeck/`
//!
//! 敏感信息（LLM API Key）单独存 `secrets.json`，权限 0600（类 Unix）。

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::ipc::dto::{AppConfig, AppConfigPatch, HostProfile};

/// 落盘结构（不含 API key 明文以外的东西；key 单独文件）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedState {
    #[serde(default)]
    config: PersistedConfig,
    #[serde(default)]
    hosts: Vec<HostProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedConfig {
    llm_provider: String,
    llm_base_url: String,
    llm_model: String,
    llm_temperature: f32,
    /// 当前生效模型 `provider/model`；空 = 走 builtin 通道
    #[serde(default)]
    active_model: String,
    /// 各渠道配置（明文 Key 单独存 secrets.json，这里只存非敏感部分）
    #[serde(default)]
    channels: Vec<crate::ipc::dto::ChannelConfig>,
    /// 命名降级链
    #[serde(default)]
    combos: Vec<crate::ipc::dto::ComboConfig>,
    terminal_font_size: u32,
    terminal_scrollback: u32,
    /// 以下字段都带 serde 默认值：老配置文件缺字段时自动补默认，不会解析失败。
    #[serde(default)]
    terminal_font_family: String,
    #[serde(default = "default_cursor_style")]
    terminal_cursor_style: String,
    #[serde(default = "default_true")]
    terminal_cursor_blink: bool,

    #[serde(default = "default_theme")]
    ui_theme: String,
    #[serde(default = "default_language")]
    ui_language: String,
    #[serde(default = "default_sidebar_width")]
    ui_sidebar_width: u32,
    #[serde(default = "default_agent_width")]
    ui_agent_panel_width: u32,
    #[serde(default = "default_true")]
    ui_show_agent_panel: bool,

    #[serde(default = "default_true")]
    behavior_confirm_on_close_tab: bool,
    #[serde(default = "default_true")]
    behavior_bell_on_disconnect: bool,
    #[serde(default)]
    behavior_auto_reconnect_attempts: u32,
    #[serde(default = "default_port")]
    behavior_default_port: u16,
    #[serde(default)]
    behavior_default_username: String,
}

fn default_cursor_style() -> String {
    "block".into()
}
fn default_true() -> bool {
    true
}
fn default_theme() -> String {
    "dark".into()
}
fn default_language() -> String {
    "zh".into()
}
fn default_sidebar_width() -> u32 {
    240
}
fn default_agent_width() -> u32 {
    384
}
fn default_port() -> u16 {
    22
}

impl Default for PersistedConfig {
    fn default() -> Self {
        Self {
            llm_provider: "openai_compat".into(),
            llm_base_url: "https://api.deepseek.com/v1".into(),
            llm_model: String::new(),
            llm_temperature: 0.2,
            active_model: String::new(),
            channels: Vec::new(),
            combos: Vec::new(),
            terminal_font_size: 14,
            terminal_scrollback: 5000,
            terminal_font_family: String::new(),
            terminal_cursor_style: default_cursor_style(),
            terminal_cursor_blink: true,
            ui_theme: default_theme(),
            ui_language: default_language(),
            ui_sidebar_width: default_sidebar_width(),
            ui_agent_panel_width: default_agent_width(),
            ui_show_agent_panel: true,
            behavior_confirm_on_close_tab: true,
            behavior_bell_on_disconnect: true,
            behavior_auto_reconnect_attempts: 0,
            behavior_default_port: default_port(),
            behavior_default_username: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Secrets {
    #[serde(default)]
    llm_api_key: String,
}

pub struct PersistStore {
    dir: PathBuf,
}

impl PersistStore {
    pub fn new() -> Self {
        let dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("modern-sshdeck");
        Self { dir }
    }

    pub fn dir(&self) -> &PathBuf {
        &self.dir
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    fn secrets_path(&self) -> PathBuf {
        self.dir.join("secrets.json")
    }

    fn ensure_dir(&self) -> AppResult<()> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| AppError::Persist(format!("create config dir: {e}")))?;
        Ok(())
    }

    // ───────────── 读取 ─────────────

    pub fn load_state(&self) -> PersistedState {
        match std::fs::read_to_string(self.state_path()) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => PersistedState::default(),
        }
    }

    pub fn load_secrets(&self) -> Secrets {
        match std::fs::read_to_string(self.secrets_path()) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Secrets::default(),
        }
    }

    /// 转为对前端的公开配置（不含明文 key，只有 `hasApiKey`）。
    pub fn public_config(&self) -> AppConfig {
        let st = self.load_state();
        let secrets = self.load_secrets();
        let c = st.config;
        AppConfig {
            llm: crate::ipc::dto::LlmConfig {
                provider: c.llm_provider,
                base_url: c.llm_base_url,
                model: c.llm_model,
                has_api_key: !secrets.llm_api_key.trim().is_empty(),
                temperature: c.llm_temperature,
                active_model: c.active_model.clone(),
                channels: c
                    .channels
                    .iter()
                    .map(|ch| crate::ipc::dto::ChannelConfig {
                        provider_id: ch.provider_id.clone(),
                        enabled: ch.enabled,
                        base_url_override: ch.base_url_override.clone(),
                        api_key: None, // 明文永不回前端
                        has_key: ch
                            .api_key
                            .as_ref()
                            .map(|k| !k.trim().is_empty())
                            .unwrap_or(false),
                        model_overrides: ch.model_overrides.clone(),
                    })
                    .collect(),
                combos: c.combos.clone(),
            },
            terminal: crate::ipc::dto::TerminalConfig {
                font_size: c.terminal_font_size,
                scrollback: c.terminal_scrollback,
                font_family: c.terminal_font_family,
                cursor_style: c.terminal_cursor_style,
                cursor_blink: c.terminal_cursor_blink,
            },
            ui: crate::ipc::dto::UiConfig {
                theme: c.ui_theme,
                language: c.ui_language,
                sidebar_width: c.ui_sidebar_width,
                agent_panel_width: c.ui_agent_panel_width,
                show_agent_panel: c.ui_show_agent_panel,
            },
            behavior: crate::ipc::dto::BehaviorConfig {
                confirm_on_close_tab: c.behavior_confirm_on_close_tab,
                bell_on_disconnect: c.behavior_bell_on_disconnect,
                auto_reconnect_attempts: c.behavior_auto_reconnect_attempts,
                default_port: c.behavior_default_port,
                default_username: c.behavior_default_username,
            },
        }
    }

    /// 供后端内部使用的明文 key。
    pub fn llm_api_key(&self) -> String {
        self.load_secrets().llm_api_key
    }

    /// 各渠道的明文 Key，仅供后端构造请求用（永不回前端）。
    pub fn channel_api_keys(&self) -> Vec<(String, String)> {
        self.load_state()
            .config
            .channels
            .into_iter()
            .filter_map(|c| {
                c.api_key
                    .filter(|k| !k.trim().is_empty())
                    .map(|k| (c.provider_id, k))
            })
            .collect()
    }

    // ───────────── 写入 ─────────────

    pub fn apply_patch(&self, patch: AppConfigPatch) -> AppResult<AppConfig> {
        self.ensure_dir()?;
        let mut st = self.load_state();

        if let Some(llm) = patch.llm {
            if let Some(v) = llm.provider {
                st.config.llm_provider = v;
            }
            if let Some(v) = llm.base_url {
                st.config.llm_base_url = v;
            }
            if let Some(v) = llm.model {
                st.config.llm_model = v;
            }
            if let Some(v) = llm.temperature {
                st.config.llm_temperature = v.clamp(0.0, 2.0);
            }
            if llm.clear_api_key {
                let mut secrets = self.load_secrets();
                secrets.llm_api_key.clear();
                self.write_json(&self.secrets_path(), &secrets, true)?;
            } else if let Some(v) = llm.api_key {
                let mut secrets = self.load_secrets();
                secrets.llm_api_key = v;
                self.write_json(&self.secrets_path(), &secrets, true)?;
            }

            // 当前生效模型：`provider/model`；空串 = 回到 builtin 通道。
            if let Some(v) = llm.active_model {
                let v = v.trim().to_string();
                if v.chars().count() <= 200 && !v.contains('\n') {
                    st.config.active_model = v;
                }
            }

            // 渠道补丁：按 provider_id 合并（未出现的渠道保持原样）。
            if let Some(patches) = llm.channels {
                for cp in patches {
                    let pid = cp.provider_id.trim().to_string();
                    if pid.is_empty() || pid.chars().count() > 64 {
                        continue;
                    }
                    let slot = match st
                        .config
                        .channels
                        .iter_mut()
                        .find(|c| c.provider_id == pid)
                    {
                        Some(existing) => existing,
                        None => {
                            st.config.channels.push(crate::ipc::dto::ChannelConfig {
                                provider_id: pid.clone(),
                                enabled: true,
                                base_url_override: None,
                                api_key: None,
                                has_key: false,
                                model_overrides: None,
                            });
                            st.config
                                .channels
                                .last_mut()
                                .expect("just pushed")
                        }
                    };

                    if let Some(v) = cp.enabled {
                        slot.enabled = v;
                    }
                    if let Some(v) = cp.base_url_override {
                        let v = v.trim().to_string();
                        slot.base_url_override = if v.is_empty() { None } else { Some(v) };
                    }
                    if cp.clear_api_key {
                        slot.api_key = None;
                        slot.has_key = false;
                    } else if let Some(v) = cp.api_key {
                        let v = v.trim().to_string();
                        slot.has_key = !v.is_empty();
                        slot.api_key = if v.is_empty() { None } else { Some(v) };
                    }
                    if let Some(v) = cp.model_overrides {
                        slot.model_overrides = if v.is_empty() { None } else { Some(v) };
                    }
                }
            }

            // 降级链全量替换；成员数上限防脏数据。
            if let Some(v) = llm.combos {
                st.config.combos = v
                    .into_iter()
                    .filter(|c| !c.name.trim().is_empty() && c.members.len() <= 16)
                    .map(|c| crate::ipc::dto::ComboConfig {
                        name: c.name.trim().to_string(),
                        strategy: if c.strategy.trim().is_empty() {
                            "fallback".into()
                        } else {
                            c.strategy.trim().to_string()
                        },
                        members: c.members,
                    })
                    .collect();
            }
        }

        if let Some(t) = patch.terminal {
            if let Some(v) = t.font_size {
                st.config.terminal_font_size = v.clamp(8, 32);
            }
            if let Some(v) = t.scrollback {
                st.config.terminal_scrollback = v.clamp(500, 100_000);
            }
            if let Some(v) = t.font_family {
                // 过长或含换行的值直接丢弃，避免污染 xterm 的 fontFamily
                let trimmed = v.trim();
                if trimmed.chars().count() <= 200 && !trimmed.contains('\n') {
                    st.config.terminal_font_family = trimmed.to_string();
                }
            }
            if let Some(v) = t.cursor_style {
                if matches!(v.as_str(), "block" | "bar" | "underline") {
                    st.config.terminal_cursor_style = v;
                }
            }
            if let Some(v) = t.cursor_blink {
                st.config.terminal_cursor_blink = v;
            }
        }

        if let Some(u) = patch.ui {
            if let Some(v) = u.theme {
                if matches!(v.as_str(), "dark" | "light" | "system") {
                    st.config.ui_theme = v;
                }
            }
            if let Some(v) = u.language {
                if matches!(v.as_str(), "zh" | "en") {
                    st.config.ui_language = v;
                }
            }
            if let Some(v) = u.sidebar_width {
                st.config.ui_sidebar_width = v.clamp(160, 480);
            }
            if let Some(v) = u.agent_panel_width {
                st.config.ui_agent_panel_width = v.clamp(260, 720);
            }
            if let Some(v) = u.show_agent_panel {
                st.config.ui_show_agent_panel = v;
            }
        }

        if let Some(b) = patch.behavior {
            if let Some(v) = b.confirm_on_close_tab {
                st.config.behavior_confirm_on_close_tab = v;
            }
            if let Some(v) = b.bell_on_disconnect {
                st.config.behavior_bell_on_disconnect = v;
            }
            if let Some(v) = b.auto_reconnect_attempts {
                st.config.behavior_auto_reconnect_attempts = v.min(10);
            }
            if let Some(v) = b.default_port {
                st.config.behavior_default_port = v;
            }
            if let Some(v) = b.default_username {
                st.config.behavior_default_username = v.trim().to_string();
            }
        }

        self.write_json(&self.state_path(), &st, false)?;
        Ok(self.public_config())
    }

    pub fn list_hosts(&self) -> Vec<HostProfile> {
        self.load_state().hosts
    }

    pub fn upsert_host(&self, host: HostProfile) -> AppResult<HostProfile> {
        self.ensure_dir()?;
        let mut st = self.load_state();
        let mut host = host;
        if host.id.trim().is_empty() {
            host.id = uuid::Uuid::new_v4().to_string();
        }
        host.updated_at = now_millis();

        match st.hosts.iter_mut().find(|h| h.id == host.id) {
            Some(existing) => *existing = host.clone(),
            None => st.hosts.push(host.clone()),
        }

        self.write_json(&self.state_path(), &st, false)?;
        Ok(host)
    }

    pub fn delete_host(&self, id: &str) -> AppResult<()> {
        let mut st = self.load_state();
        let before = st.hosts.len();
        st.hosts.retain(|h| h.id != id);
        if st.hosts.len() == before {
            return Err(AppError::NotFound(format!("host {id}")));
        }
        self.write_json(&self.state_path(), &st, false)?;
        Ok(())
    }

    /// 原子写：先写临时文件再重命名，避免半写状态。
    fn write_json<T: Serialize>(&self, path: &PathBuf, value: &T, secret: bool) -> AppResult<()> {
        self.ensure_dir()?;
        let raw = serde_json::to_string_pretty(value)?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, raw.as_bytes())
            .map_err(|e| AppError::Persist(format!("write tmp: {e}")))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| AppError::Persist(format!("rename: {e}")))?;
        if secret {
            restrict_permissions(path);
        }
        Ok(())
    }
}

impl Default for PersistStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &PathBuf) {
    // Windows 上依赖用户目录 ACL；不额外处理。
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 供其他模块使用的空标记，避免未使用告警。
pub type _HostMap = HashMap<String, HostProfile>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::dto::{AppConfigPatch, BehaviorConfigPatch, TerminalConfigPatch, UiConfigPatch};

    /// 用临时目录构造独立 store，避免污染真实用户配置。
    fn temp_store(tag: &str) -> PersistStore {
        let dir = std::env::temp_dir().join(format!("sshdeck-test-{tag}-{}", uuid::Uuid::new_v4()));
        PersistStore { dir }
    }

    #[test]
    fn defaults_are_sane() {
        let store = temp_store("defaults");
        let cfg = store.public_config();

        assert_eq!(cfg.terminal.font_size, 14);
        assert_eq!(cfg.terminal.scrollback, 5000);
        assert_eq!(cfg.terminal.font_family, "");
        assert_eq!(cfg.terminal.cursor_style, "block");
        assert!(cfg.terminal.cursor_blink);

        assert_eq!(cfg.ui.theme, "dark");
        assert_eq!(cfg.ui.language, "zh");
        assert_eq!(cfg.ui.sidebar_width, 240);
        assert_eq!(cfg.ui.agent_panel_width, 384);
        assert!(cfg.ui.show_agent_panel);

        assert_eq!(cfg.behavior.default_port, 22);
        assert!(cfg.behavior.confirm_on_close_tab);
        assert!(!cfg.llm.has_api_key);
    }

    #[test]
    fn terminal_appearance_roundtrip() {
        let store = temp_store("terminal");
        store
            .apply_patch(AppConfigPatch {
                terminal: Some(TerminalConfigPatch {
                    font_size: Some(18),
                    scrollback: Some(20000),
                    font_family: Some("  \"MesloLGS NF\", Consolas  ".into()),
                    cursor_style: Some("bar".into()),
                    cursor_blink: Some(false),
                }),
                ..Default::default()
            })
            .expect("apply");

        let cfg = store.public_config();
        assert_eq!(cfg.terminal.font_size, 18);
        assert_eq!(cfg.terminal.scrollback, 20000);
        // 字体族会被 trim
        assert_eq!(cfg.terminal.font_family, "\"MesloLGS NF\", Consolas");
        assert_eq!(cfg.terminal.cursor_style, "bar");
        assert!(!cfg.terminal.cursor_blink);
    }

    #[test]
    fn invalid_values_are_rejected_or_clamped() {
        let store = temp_store("validate");
        store
            .apply_patch(AppConfigPatch {
                terminal: Some(TerminalConfigPatch {
                    font_size: Some(999),                // 超出范围 → 夹到 32
                    scrollback: Some(1),                 // 低于下限 → 夹到 500
                    font_family: Some("bad\nnewline".into()), // 含换行 → 拒绝写入
                    cursor_style: Some("nonsense".into()),    // 非法枚举 → 忽略
                    cursor_blink: None,
                }),
                ui: Some(UiConfigPatch {
                    theme: Some("rainbow".into()),       // 非法主题 → 忽略
                    language: Some("fr".into()),         // 不支持的语言 → 忽略
                    sidebar_width: Some(10),             // 夹到 160
                    agent_panel_width: Some(9999),       // 夹到 720
                    show_agent_panel: Some(false),
                }),
                behavior: Some(BehaviorConfigPatch {
                    auto_reconnect_attempts: Some(999),  // 上限 10
                    default_port: Some(0),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .expect("apply");

        let cfg = store.public_config();
        assert_eq!(cfg.terminal.font_size, 32, "font size clamped to max");
        assert_eq!(cfg.terminal.scrollback, 500, "scrollback clamped to min");
        assert_eq!(cfg.terminal.font_family, "", "newline font family rejected");
        assert_eq!(cfg.terminal.cursor_style, "block", "bad cursor style ignored");

        assert_eq!(cfg.ui.theme, "dark", "bad theme ignored");
        assert_eq!(cfg.ui.language, "zh", "unsupported language ignored");
        assert_eq!(cfg.ui.sidebar_width, 160);
        assert_eq!(cfg.ui.agent_panel_width, 720);
        assert!(!cfg.ui.show_agent_panel);

        assert_eq!(cfg.behavior.auto_reconnect_attempts, 10);
    }

    #[test]
    fn state_survives_reload() {
        let store = temp_store("reload");
        store
            .apply_patch(AppConfigPatch {
                terminal: Some(TerminalConfigPatch {
                    font_size: Some(16),
                    ..Default::default()
                }),
                ui: Some(UiConfigPatch {
                    theme: Some("light".into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .expect("apply");

        // 同一个磁盘目录重新构造 store，模拟应用重启
        let reopened = PersistStore {
            dir: store.dir.clone(),
        };
        let cfg = reopened.public_config();
        assert_eq!(cfg.terminal.font_size, 16, "terminal font size persisted");
        assert_eq!(cfg.ui.theme, "light", "theme persisted");
    }

    #[test]
    fn legacy_state_without_new_fields_still_loads() {
        // 老版本 state.json 只有少数字段；必须能用默认值补齐而不是解析失败。
        let store = temp_store("legacy");
        std::fs::create_dir_all(&store.dir).expect("mkdir");
        let legacy = serde_json::json!({
            "config": {
                "llmProvider": "openai_compat",
                "llmBaseUrl": "https://api.deepseek.com/v1",
                "llmModel": "",
                "llmTemperature": 0.2,
                "terminalFontSize": 15,
                "terminalScrollback": 5000,
                "uiTheme": "light"
            },
            "hosts": []
        });
        std::fs::write(
            store.dir.join("state.json"),
            serde_json::to_string_pretty(&legacy).unwrap(),
        )
        .expect("write legacy");

        let cfg = store.public_config();
        // 老字段保留
        assert_eq!(cfg.terminal.font_size, 15);
        assert_eq!(cfg.ui.theme, "light");
        // 新字段补默认
        assert_eq!(cfg.terminal.font_family, "");
        assert_eq!(cfg.terminal.cursor_style, "block");
        assert_eq!(cfg.ui.sidebar_width, 240);
        assert_eq!(cfg.behavior.default_port, 22);
    }
}
