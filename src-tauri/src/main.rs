#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod commands;
mod llm;
mod policy;
mod scheduler;
mod store;
mod tools;

use tauri::Manager;
use tracing::info;

pub use store::AppState;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "pisci_desktop=info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let app_handle = app.handle().clone();

            // Initialize application state (SQLite + config)
            let state = store::AppState::new(&app_handle)?;
            app.manage(state);

            info!("Pisci Desktop started");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Settings
            commands::settings::get_settings,
            commands::settings::save_settings,
            commands::settings::is_configured,
            // Sessions
            commands::chat::create_session,
            commands::chat::list_sessions,
            commands::chat::delete_session,
            commands::chat::get_messages,
            commands::chat::chat_send,
            commands::chat::chat_cancel,
            // Memory
            commands::memory::list_memories,
            commands::memory::add_memory,
            commands::memory::delete_memory,
            commands::memory::clear_memories,
            // Skills
            commands::skills::list_skills,
            commands::skills::toggle_skill,
            // Scheduler
            commands::scheduler::list_tasks,
            commands::scheduler::create_task,
            commands::scheduler::update_task,
            commands::scheduler::delete_task,
            commands::scheduler::run_task_now,
            // System
            commands::system::get_vm_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pisci Desktop");
}
