//! 全局应用状态。
//!
//! 由 `lib.rs` 在启动时构造并 `manage`，各 Tauri command 通过 `State<AppState>` 访问。

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::agent::runtime::AgentRuntime;
use crate::forward::registry::ForwardRegistry;
use crate::ipc::dto::AppConfig;
use crate::persist::store::PersistStore;
use crate::ssh::manager::SessionManager;

pub struct AppState {
    pub persist: PersistStore,
    pub sessions: Arc<SessionManager>,
    pub forwards: Arc<ForwardRegistry>,
    pub agent: Arc<AgentRuntime>,
    /// 配置缓存；写入时同步更新
    pub config: Mutex<AppConfig>,
}

impl AppState {
    pub fn new() -> Self {
        let persist = PersistStore::new();
        let config = persist.public_config();
        let sessions = Arc::new(SessionManager::new());
        let forwards = Arc::new(ForwardRegistry::new(sessions.clone()));
        let agent = Arc::new(AgentRuntime::new(sessions.clone(), persist.dir().clone()));

        Self {
            persist,
            sessions,
            forwards,
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
