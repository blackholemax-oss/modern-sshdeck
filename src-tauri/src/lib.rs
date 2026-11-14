//! 应用入口。

pub mod agent;
pub mod error;
pub mod forward;
pub mod guard;
pub mod ipc;
pub mod llm;
pub mod persist;
pub mod ssh;
pub mod state;
pub mod token;

pub use error::{AppError, AppResult};
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "modern_sshdeck_lib=info,warn".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let state = state::AppState::new();

            // 连接复用：起空闲连接回收任务
            state.sessions.spawn_link_reaper();

            // 转发守护：挂上 AppHandle 后恢复持久化规则（重启后自动拉起）
            state.supervisor.attach_app(app.handle().clone());
            let supervisor = state.supervisor.clone();
            tauri::async_runtime::spawn(async move {
                supervisor.restore_all().await;
            });

            app.manage(state);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::app_get_config,
            ipc::app_set_config,
            ipc::hosts_list,
            ipc::hosts_upsert,
            ipc::hosts_delete,
            ipc::ssh_connect,
            ipc::ssh_disconnect,
            ipc::ssh_list_sessions,
            ipc::ssh_probe_host_key,
            ipc::ssh_pool_stats,
            ipc::terminal_input,
            ipc::terminal_resize,
            ipc::fs::fs_list_dir,
            ipc::fs::fs_read_file,
            ipc::fs::fs_download,
            ipc::fs::fs_upload,
            ipc::forward_start,
            ipc::forward_stop,
            ipc::forward_get,
            ipc::forward_list,
            ipc::forward_update,
            ipc::forward_retry,
            ipc::agent_send,
            ipc::agent_abort,
            ipc::agent_approve,
            ipc::agent_list_models,
            ipc::agent_reset,
            ipc::extras::llm_providers,
            ipc::extras::llm_probe_channel,
            ipc::extras::llm_discover_models,
            ipc::extras::token_stats,
            ipc::extras::token_stats_reset,
            ipc::extras::audit_tail,
            ipc::extras::audit_count,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
