#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use chrono::Utc;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::Manager;
use wallet::account;
use wallet::constants::WALLET_DIR;
use wallet::core::Core;
use wallet::network;
mod wallet;

struct AppState {
    core: Arc<Core>,
    config_path: PathBuf,
}

#[derive(Serialize)]
pub struct ChartDataPoint {
    pub timestamp: i64,
    pub balance: f64,
}

#[tauri::command]
async fn list_wallets(state: tauri::State<'_, AppState>) -> Result<Vec<String>, String> {
    let mut wallet_names = Vec::new();
    let wallets_dir = state.config_path.parent().unwrap().join(WALLET_DIR);

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
    account::create_wallet(&state.core, &name, &password, &state.config_path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn recover_wallet_command(
    state: tauri::State<'_, AppState>,
    name: String,
    password: String,
    seed_phrase: String,
) -> Result<(), String> {
    crate::wallet::account::recover_wallet_with_seed(
        &state.core,
        &name,
        &password,
        &seed_phrase,
        &state.config_path,
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn recover_from_pk_command(
    state: tauri::State<'_, AppState>,
    name: String,
    password: String,
    private_key: String,
) -> Result<(), String> {
    crate::wallet::account::recover_wallet_with_key(
        &state.core,
        &name,
        &password,
        &private_key,
        &state.config_path,
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn unlock_wallet(
    state: tauri::State<'_, AppState>,
    name: String,
    password: String,
) -> Result<(), String> {
    account::load_wallet(&state.core, &name, &password, &state.config_path)
        .await
        .map_err(|e| format!("Authentication failed: {}", e))
}

#[tauri::command]
async fn get_wallet_state(
    state: tauri::State<'_, AppState>,
) -> Result<wisp_core::network::WalletStateSnapshot, String> {
    let transactions_guard = state.core.transactions.read().await;
    let utxos_guard = state.core.utxos.read().await;

    let snapshot = wisp_core::network::WalletStateSnapshot {
        transactions: transactions_guard.values().cloned().collect(),
        utxos: utxos_guard.iter().map(|(k, v)| (*k, v.clone())).collect(),
    };

    Ok(snapshot)
}

#[tauri::command]
async fn get_current_wallet_info(
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let wallet = crate::wallet::account::get_current_wallet(&state.core)
        .await
        .map_err(|e| e.to_string())?;

    // Convert the public key to hex string
    let pk_hex = hex::encode(wallet.public_key.0.to_encoded_point(true).as_bytes());

    Ok(serde_json::json!({
        "name": wallet.name,
        "public_key": pk_hex,
    }))
}

#[tauri::command]
async fn refresh_wallet(state: tauri::State<'_, AppState>) -> Result<(), String> {
    network::fetch_wallet_state(&state.core)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_wallet_balance(state: tauri::State<'_, AppState>) -> Result<u64, String> {
    let utxos_guard = state.core.utxos.read().await;
    let total_balance: u64 = utxos_guard
        .values()
        .map(|output| output.value.as_smallest_unit())
        .sum();

    Ok(total_balance)
}

#[tauri::command]
async fn get_balance_history(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ChartDataPoint>, String> {
    let txs_guard = state.core.transactions.read().await;
    let wallet_pk = crate::wallet::account::get_current_wallet(&state.core)
        .await
        .map_err(|e| e.to_string())?
        .public_key;

    let mut history: Vec<_> = txs_guard.values().cloned().collect();
    history.sort_by(|a, b| {
        a.block_index
            .unwrap_or(u64::MAX)
            .cmp(&b.block_index.unwrap_or(u64::MAX))
    });

    let mut running_balance: i128 = 0;
    let mut chart_data = Vec::new();

    for tx_info in history {
        let mut tx_net_change: i128 = 0;

        for output in &tx_info.transaction.outputs {
            if output.pubkey == wallet_pk {
                tx_net_change += output.value.as_smallest_unit() as i128;
            }
        }

        for input in &tx_info.transaction.inputs {
            if let Some(prev_tx_info) = txs_guard.get(&input.outpoint.txid) {
                if let Some(prev_output) = prev_tx_info
                    .transaction
                    .outputs
                    .get(input.outpoint.vout as usize)
                {
                    if prev_output.pubkey == wallet_pk {
                        tx_net_change -= prev_output.value.as_smallest_unit() as i128;
                    }
                }
            }
        }

        running_balance += tx_net_change;

        chart_data.push(ChartDataPoint {
            timestamp: tx_info
                .block_timestamp
                .map(|dt| dt.timestamp())
                .unwrap_or_else(|| Utc::now().timestamp()),
            balance: (running_balance as f64) / 100_000_000.0,
        });
    }

    Ok(chart_data)
}

use serde_json::json;

#[tauri::command]
async fn get_recent_transactions(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let tx_guard = state.core.transactions.read().await;
    let wallet = crate::wallet::account::get_current_wallet(&state.core)
        .await
        .map_err(|e| e.to_string())?;

    // Use the actual public key from the loaded wallet
    let wallet_pk = wallet.public_key;

    let mut enriched_txs = Vec::new();

    for tx_info in tx_guard.values() {
        let mut inputs_json = Vec::new();

        for input in &tx_info.transaction.inputs {
            let mut prev_out_json = json!(null);
            if let Some(prev_tx_info) = tx_guard.get(&input.outpoint.txid) {
                if let Some(output) = prev_tx_info
                    .transaction
                    .outputs
                    .get(input.outpoint.vout as usize)
                {
                    // CRITICAL: Compare the pubkey here
                    prev_out_json = json!({
                        "pubkey": hex::encode(output.pubkey.0.to_encoded_point(true).as_bytes()),
                        "value": output.value.as_smallest_unit(),
                        "is_ours": output.pubkey == wallet_pk // Now wallet_pk is used!
                    });
                }
            }
            inputs_json.push(json!({ "previous_output": prev_out_json }));
        }

        let outputs_json: Vec<_> = tx_info
            .transaction
            .outputs
            .iter()
            .map(|o| {
                json!({
                    "pubkey": hex::encode(o.pubkey.0.to_encoded_point(true).as_bytes()),
                    "value": o.value.as_smallest_unit(),
                    "is_ours": o.pubkey == wallet_pk // Now wallet_pk is used!
                })
            })
            .collect();

        let tx_id = tx_info
            .transaction
            .txid()
            .map_err(|e| e.to_string())?
            .to_string();

        enriched_txs.push(json!({
            "transaction": {
                "id": tx_id,
                "inputs": inputs_json,
                "outputs": outputs_json,
            },
            "status": tx_info.status,
            "block_timestamp": tx_info.block_timestamp,
        }));
    }

    // Sort by timestamp descending
    enriched_txs.sort_by(|a, b| {
        b["block_timestamp"]
            .as_str()
            .cmp(&a["block_timestamp"].as_str())
    });

    Ok(enriched_txs)
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
                network::start_background_sync(sync_core).await;
            });

            app.manage(AppState {
                core: core_arc,
                config_path,
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_wallet_state,
            list_wallets,
            create_new_wallet,
            recover_wallet_command,
            recover_from_pk_command,
            unlock_wallet,
            refresh_wallet,
            get_wallet_balance,
            get_recent_transactions,
            get_balance_history,
            get_current_wallet_info
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
