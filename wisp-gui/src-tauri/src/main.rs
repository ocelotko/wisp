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
use wallet::types::FeeType;
use wisp_core::currency::Amount;
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
    account::load_wallet(
        Arc::clone(&state.core),
        &name,
        &password,
        &state.config_path,
    )
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
fn validate_address(address: String) -> bool {
    wisp_core::address::Address::decode(&address).is_ok()
}

#[tauri::command]
async fn get_node_status(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    let config = state.core.config.lock().await;
    let stream = state.core.connected_node_stream.lock().await;
    Ok(serde_json::json!({
        "address": config.default_node,
        "is_connected": stream.is_some(),
    }))
}

#[tauri::command]
async fn set_default_node_command(
    state: tauri::State<'_, AppState>,
    node_address: String,
) -> Result<(), String> {
    network::set_default_node(&state.core, &node_address, &state.config_path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_wallet_command(
    state: tauri::State<'_, AppState>,
    name: String,
) -> Result<(), String> {
    account::delete_wallet(&state.core, &name, &state.config_path)
        .await
        .map_err(|e| e.to_string())
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

    let first_aurora_address = crate::wallet::account::get_receive_addresses(&wallet)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|(label, _)| label.contains("Aurora"))
        .map(|(_, addr)| addr);

    Ok(serde_json::json!({
        "name": wallet.name,
        "public_key": pk_hex,
        "address_count": wallet.derived_public_keys.len(),
        "first_aurora_address": first_aurora_address,
    }))
}

#[tauri::command]
async fn get_wallet_addresses(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let wallet = crate::wallet::account::get_current_wallet(&state.core)
        .await
        .map_err(|e| e.to_string())?;

    let addresses =
        crate::wallet::account::get_receive_addresses(&wallet).map_err(|e| e.to_string())?;

    Ok(addresses
        .into_iter()
        .map(|(label, address)| serde_json::json!({ "label": label, "address": address }))
        .collect())
}

#[tauri::command]
async fn change_password_command(
    state: tauri::State<'_, AppState>,
    current_password: String,
    new_password: String,
) -> Result<(), String> {
    crate::wallet::account::change_wallet_password(&state.core, &current_password, &new_password)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn export_seed_command(
    state: tauri::State<'_, AppState>,
    password: String,
) -> Result<String, String> {
    crate::wallet::account::export_seed_phrase(&state.core, &password)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn generate_new_address_command(
    state: tauri::State<'_, AppState>,
    password: String,
) -> Result<u32, String> {
    crate::wallet::account::generate_new_address(Arc::clone(&state.core), &password)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn refresh_wallet(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let core_clone = Arc::clone(&state.core);
    tokio::spawn(async move {
        if let Err(e) = network::fetch_wallet_state(&core_clone).await {
            log::error!("Manual refresh failed: {}", e);
        }
    });
    Ok(())
}

#[tauri::command]
async fn get_wallet_summary(
    state: tauri::State<'_, AppState>,
) -> Result<crate::wallet::transaction::WalletSummary, String> {
    crate::wallet::transaction::get_wallet_summary(&state.core)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn send_funds_command(
    state: tauri::State<'_, AppState>,
    is_send_max: bool,
    recipient_public_key_str: String,
    amount_str: String, // amount in WISP as a string, e.g., "1.23"
    fee_type: FeeType,
    fee_value_raw: u64,
    password: String,
) -> Result<(), String> {
    let amount_to_send = if is_send_max {
        Amount::zero() // It will be recalculated inside send_funds
    } else {
        // Parse from string like "1.23" to smallest unit
        let amount_f64 = amount_str
            .parse::<f64>()
            .map_err(|_| "Invalid amount format. Must be a number.".to_string())?;
        if amount_f64 < 0.0 {
            return Err("Amount cannot be negative.".to_string());
        }
        let amount_smallest_unit = (amount_f64 * 100_000_000.0).round() as u64;
        Amount::from_smallest_unit(amount_smallest_unit)
    };

    wallet::transaction::send_funds(
        &state.core,
        is_send_max,
        recipient_public_key_str,
        amount_to_send,
        fee_type,
        fee_value_raw,
        &password,
        &state.config_path,
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_balance_history(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ChartDataPoint>, String> {
    let txs_guard = state.core.transactions.read().await;
    let wallet = crate::wallet::account::get_current_wallet(&state.core)
        .await
        .map_err(|e| e.to_string())?;

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
            if wallet.is_script_relevant(&output.script) {
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
                    if wallet.is_script_relevant(&prev_output.script) {
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

    let mut enriched_txs = Vec::new();

    for tx_info in tx_guard.values() {
        let mut value_from_us: u64 = 0;
        let mut value_to_us: u64 = 0;
        let mut inputs_json = Vec::new();

        for input in &tx_info.transaction.inputs {
            let mut prev_out_json = json!(null);
            if let Some(prev_tx_info) = tx_guard.get(&input.outpoint.txid) {
                if let Some(output) = prev_tx_info
                    .transaction
                    .outputs
                    .get(input.outpoint.vout as usize)
                {
                    let is_ours = wallet.is_script_relevant(&output.script);
                    if is_ours {
                        value_from_us += output.value.as_smallest_unit();
                    }
                    prev_out_json = json!({
                        "address": wisp_core::address::Address::encode(&output.script),
                        "value": output.value.as_smallest_unit(),
                        "is_ours": is_ours,
                    });
                }
            }
            inputs_json.push(json!({ "previous_output": prev_out_json }));
        }

        let mut outputs_json = Vec::new();
        for o in &tx_info.transaction.outputs {
            let is_ours = wallet.is_script_relevant(&o.script);
            if is_ours {
                value_to_us += o.value.as_smallest_unit();
            }
            outputs_json.push(json!({
                "address": wisp_core::address::Address::encode(&o.script),
                "value": o.value.as_smallest_unit(),
                "is_ours": is_ours
            }));
        }

        // Determine the "UX" type of the transaction
        let net_change = value_to_us as i128 - value_from_us as i128;
        let (display_type, display_amount) = if tx_info.transaction.is_coinbase() {
            ("Mining Reward", value_to_us)
        } else if net_change < 0 {
            // We sent more than we received (Outbound)
            // The actual amount sent is the sum of outputs that are NOT ours
            let sent_amount: u64 = tx_info
                .transaction
                .outputs
                .iter()
                .filter(|o| !wallet.is_script_relevant(&o.script))
                .map(|o| o.value.as_smallest_unit())
                .sum();
            if sent_amount == 0 {
                ("Self Transfer", (net_change.abs() as u64))
            } else {
                ("Sent", sent_amount)
            }
        } else {
            // We received more than we spent (Inbound)
            ("Received", net_change.abs() as u64)
        };

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
                "display_type": display_type,
                "net_value": display_amount,
            },
            "status": tx_info.status,
            "block_timestamp": tx_info.block_timestamp,
        }));
    }

    // Sort by timestamp descending
    enriched_txs.sort_by(|a, b| {
        let time_a = a["block_timestamp"].as_str().unwrap_or("");
        let time_b = b["block_timestamp"].as_str().unwrap_or("");
        // Newest first
        time_b.cmp(time_a)
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
            get_wallet_summary,
            get_recent_transactions,
            get_balance_history,
            get_current_wallet_info,
            send_funds_command,
            get_wallet_addresses,
            generate_new_address_command,
            change_password_command,
            export_seed_command,
            validate_address,
            get_node_status,
            set_default_node_command,
            delete_wallet_command
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
