// wisp-gui/src-tauri/src/main.rs
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::path::PathBuf;
use std::sync::Arc;
use tauri::Manager;
use wallet::core::Core;
mod wallet;

struct AppState {
    core: Arc<Core>,
    config_path: PathBuf,
}

#[tauri::command]
async fn list_wallets(state: tauri::State<'_, AppState>) -> Result<Vec<String>, String> {
    let mut wallet_names = Vec::new();
    let wallets_dir = state.config_path.parent().unwrap().join("wallets");

    if let Ok(entries) = std::fs::read_dir(wallets_dir) {
        for entry in entries.flatten() {
            if let Some(ext) = entry.path().extension() {
                if ext == "dat" {
                    if let Some(name) = entry.path().file_stem() {
                        wallet_names.push(name.to_string_lossy().into_owned());
                    }
                }
            }
        }
    }
    Ok(wallet_names)
}

#[tauri::command]
async fn create_new_wallet(
    state: tauri::State<'_, AppState>,
    name: String,
    password: String,
) -> Result<String, String> {
    state
        .core
        .create_wallet(&name, &password, &state.config_path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn unlock_wallet(
    state: tauri::State<'_, AppState>,
    name: String,
    password: String,
) -> Result<(), String> {
    state
        .core
        .load_wallet(&name, &password)
        .await
        .map_err(|e| format!("Authentication failed: {}", e))
}

#[tauri::command]
async fn refresh_wallet(state: tauri::State<'_, Arc<Core>>) -> Result<(), String> {
    state.fetch_wallet_state().await.map_err(|e| e.to_string())
}

fn main() {
    env_logger::init();

    tauri::Builder::default()
        .setup(|app| {
            let app_data_dir = app
                .path()
                .app_data_dir()
                .expect("Failed to get app data directory");

            std::fs::create_dir_all(&app_data_dir).ok();

            let config_path = app_data_dir.join("config.toml");
            let core = tauri::async_runtime::block_on(async {
                Core::load(config_path.clone())
                    .await
                    .expect("Failed to load wallet core")
            });

            let core_arc = Arc::new(core);
            let sync_core = Arc::clone(&core_arc);
            tauri::async_runtime::spawn(async move {
                sync_core.start_background_sync().await;
            });

            app.manage(AppState {
                core: core_arc,
                config_path,
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            refresh_wallet,
            create_new_wallet,
            unlock_wallet,
            list_wallets
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
