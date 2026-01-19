// wisp-gui/src-tauri/src/main.rs
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use chrono::Utc;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;
use wisp_core::{currency::Amount, network::TransactionStatus};

// Import the wallet module we copied
mod wallet;
use wallet::core::Core;

// Define the application state to hold the Wallet Core
struct AppState {
    core: Arc<Core>,
    config_path: PathBuf,
}

#[derive(serde::Serialize)]
struct TransactionDto {
    id: String,
    kind: String, // "Sent", "Received", "Coinbase"
    amount: String,
    timestamp: String,
    status: String,
}

// --- Tauri Commands ---

#[tauri::command]
async fn create_wallet(
    name: String,
    password: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    state
        .core
        .create_wallet(&name, &password, &state.config_path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn load_wallet(
    name: String,
    password: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .core
        .load_wallet(&name, &password, &state.config_path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_balance(state: State<'_, AppState>) -> Result<String, String> {
    let balance = state
        .core
        .get_total_balance()
        .await
        .map_err(|e| e.to_string())?;
    Ok(balance.to_string_wisp())
}

#[tauri::command]
async fn get_address(state: State<'_, AppState>) -> Result<String, String> {
    let wallet = state
        .core
        .get_current_wallet()
        .await
        .map_err(|e| e.to_string())?;
    Ok(wallet.public_key.fingerprint())
}

#[tauri::command]
async fn list_wallets() -> Result<Vec<String>, String> {
    Core::load_wallets().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_transactions(state: State<'_, AppState>) -> Result<Vec<TransactionDto>, String> {
    let core = &state.core;
    let current_wallet = core.get_current_wallet().await.map_err(|e| e.to_string())?;
    let wallet_public_key = current_wallet.public_key;

    let all_txs = core.transactions.read().await;
    let mut dtos = Vec::new();

    for tx_info in all_txs.values() {
        let tx = &tx_info.transaction;
        let txid = match tx.txid() {
            Ok(id) => id.to_string(),
            Err(_) => continue,
        };

        let mut value_from_us = Amount::zero();
        let mut value_to_us = Amount::zero();

        // Calculate inputs (money sent by us)
        for input in &tx.inputs {
            if let Some(source_tx_info) = all_txs.get(&input.outpoint.txid) {
                if let Some(spent_output) = source_tx_info
                    .transaction
                    .outputs
                    .get(input.outpoint.vout as usize)
                {
                    if spent_output.pubkey == wallet_public_key {
                        if let Some(new_val) = value_from_us.checked_add(spent_output.value) {
                            value_from_us = new_val;
                        }
                    }
                }
            }
        }

        // Calculate outputs (money received by us)
        for output in &tx.outputs {
            if output.pubkey == wallet_public_key {
                if let Some(new_val) = value_to_us.checked_add(output.value) {
                    value_to_us = new_val;
                }
            }
        }

        if value_from_us == Amount::zero() && value_to_us == Amount::zero() {
            continue;
        }

        let net_diff =
            (value_to_us.as_smallest_unit() as i128) - (value_from_us.as_smallest_unit() as i128);

        let kind = if tx.is_coinbase() {
            "Coinbase"
        } else if net_diff < 0 {
            "Sent"
        } else {
            "Received"
        };
        let abs_diff = net_diff.abs() as u64;
        let amount_obj = Amount::from_smallest_unit(abs_diff);
        let prefix = if net_diff < 0 { "-" } else { "+" };

        dtos.push(TransactionDto {
            id: txid,
            kind: kind.to_string(),
            amount: format!("{} {}", prefix, amount_obj.to_string_wisp()),
            timestamp: tx_info
                .block_timestamp
                .unwrap_or_else(Utc::now)
                .to_rfc3339(),
            status: if tx_info.status == TransactionStatus::Pending {
                "Pending".to_string()
            } else {
                "Confirmed".to_string()
            },
        });
    }

    // Sort by timestamp descending
    dtos.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(dtos)
}

#[tauri::command]
async fn send_funds(
    recipient: String,
    amount_wisp: String,
    fee_fixed: u64,
    password: String,
    is_send_max: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    use wallet::core::FeeType;

    let amount = if is_send_max {
        Amount::zero()
    } else {
        Amount::from_string_wisp(&amount_wisp).map_err(|e| e.to_string())?
    };

    state
        .core
        .send_funds(
            is_send_max,
            recipient,
            amount,
            FeeType::Fixed,
            fee_fixed,
            &password,
            &state.config_path,
        )
        .await
        .map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() {
    env_logger::init();

    // Determine a path for the config file (e.g., in the current directory for now)
    let config_path = PathBuf::from("wisp_gui_config.toml");

    // Initialize the Core
    let core = match Core::load(config_path.clone()).await {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("Failed to load wallet core: {}", e);
            return;
        }
    };

    // Start background sync
    Arc::clone(&core).start_background_sync().await;

    tauri::Builder::default()
        .manage(AppState { core, config_path })
        .invoke_handler(tauri::generate_handler![
            create_wallet,
            load_wallet,
            get_balance,
            get_address,
            list_wallets,
            send_funds,
            get_transactions
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
