//! 全局应用状态。
//!
//! 由 `lib.rs` 在启动时构造并 `manage`，各 Tauri command 通过 `State<AppState>` 访问。

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::agent::runtime::AgentRuntime;
use crate::forward::registry::ForwardRegistry;
use crate::forward::supervisor::ForwardSupervisor;
use crate::ipc::dto::AppConfig;
use crate::persist::store::PersistStore;
use crate::ssh::manager::SessionManager;

pub struct AppState {
    pub persist: Arc<PersistStore>,
    pub sessions: Arc<SessionManager>,
    pub forwards: Arc<ForwardRegistry>,
    /// 转发守护：持久化 + 断线 / 重启自动恢复
    pub supervisor: Arc<ForwardSupervisor>,
    pub agent: Arc<AgentRuntime>,
    /// 配置缓存；写入时同步更新
    pub config: Mutex<AppConfig>,
}

impl AppState {
    pub fn new() -> Self {
        let persist = Arc::new(PersistStore::new());
        let config = persist.public_config();
        let sessions = Arc::new(SessionManager::new());
        // 主机密钥校验策略来自用户配置：strict（默认）/ accept_new / insecure
        sessions.apply_host_key_settings(
            &config.behavior.host_key_policy,
            &config.behavior.known_hosts_path,
        );
        // 连接复用：默认开启，空闲 300s 回收
        sessions.apply_reuse_settings(
            config.behavior.session_reuse,
            config.behavior.session_reuse_idle_secs,
        );
        let forwards = Arc::new(ForwardRegistry::new(sessions.clone()));
        let supervisor = Arc::new(ForwardSupervisor::new(
            sessions.clone(),
            forwards.clone(),
            persist.clone(),
        ));
        supervisor.apply_settings(
            config.behavior.forward_auto_restore,
            config.behavior.forward_reconnect_max_secs,
        );
        let agent = Arc::new(AgentRuntime::new(sessions.clone(), persist.dir().clone()));

        Self {
            persist,
            sessions,
            forwards,
            supervisor,
            agent,
            config: Mutex::new(config),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
