use crate::utils::{
    clear_terminal, display_heading, display_heading_with_wallet, pause, prompt_password,
};
use crate::wallet::core::Core;
use anyhow::{Context, Result};
use inquire::{Confirm, Select};
use log::{error, info};
use std::path::PathBuf;
use std::{collections::HashSet, sync::Arc};
use wisp_core::{currency::Amount, network::TransactionStatus};

pub async fn settings_and_info(
    core: Arc<Core>,
    config_path: &PathBuf,
) -> Result<(), anyhow::Error> {
    loop {
        clear_terminal();
        let balance_value = core.get_total_balance().await;
        display_heading_with_wallet(
            core.get_current_wallet()
                .await
                .ok()
                .map(|w| w.name)
                .as_deref(),
            balance_value.ok(),
        );
        let utilities_options = vec![
            "Get wallet and node info",
            "Delete wallet",
            "Back to wallet menu",
        ];
        let utilities_menu_selection =
            Select::new("Utilities and Information", utilities_options).prompt()?;

        match utilities_menu_selection.as_ref() {
            "Get wallet and node info" => get_info(&core).await?,
            "Delete wallet" => {
                if delete_current_wallet_prompt(Arc::clone(&core), config_path)
                    .await
                    .context("Failed to delete wallet")?
                {
                    return Ok(());
                }
            }
            "Back to wallet menu" => return Ok(()),
            _ => {
                error!("Utilities menu selection error: Invalid selection or prompt error.");
                println!("Invalid selection or prompt error.");
                pause();
            }
        }
    }
}

async fn get_info(core: &Core) -> Result<(), anyhow::Error> {
    clear_terminal();
    let balance_value = core.get_total_balance().await;
    display_heading_with_wallet(
        core.get_current_wallet()
            .await
            .ok()
            .map(|w| w.name)
            .as_deref(),
        balance_value.ok(),
    );

    match core.get_current_wallet().await {
        Ok(wallet) => {
            println!("\n--- Wallet Info ---");
            println!("Wallet Name:         {}", wallet.name);
            println!("Public Key:          {}", wallet.public_key.fingerprint());

            println!("\n--- Network Info ---");
            let config_guard = core.config.lock().await;
            println!("Connected Node:      {}", config_guard.default_node);
            drop(config_guard);

            let discovered_nodes_guard = core.discovered_nodes.lock().await;
            let total_discovered_nodes = discovered_nodes_guard.len();
            println!("Known Peers:         {}", total_discovered_nodes);

            println!("\n--- Balance & State ---");
            let transactions_guard = core.transactions.read().await;
            let utxos_guard = core.utxos.read().await;

            let available_balance = utxos_guard
                .values()
                .fold(Amount::zero(), |acc, output| acc + output.value)
                .to_string_wisp();

            let mut pending_change = 0i64;
            let mut pending_tx_count = 0;
            let wallet_utxos_set: HashSet<_> = utxos_guard.keys().collect();

            for tx_info in transactions_guard.values() {
                if tx_info.status == TransactionStatus::Pending {
                    pending_tx_count += 1;
                    // Sum inputs that were part of our available UTXOs
                    for input in &tx_info.transaction.inputs {
                        if wallet_utxos_set.contains(&input.outpoint) {
                            if let Some(utxo_output) = utxos_guard.get(&input.outpoint) {
                                pending_change -= utxo_output.value.as_smallest_unit() as i64;
                            }
                        }
                    }
                    for output in &tx_info.transaction.outputs {
                        if output.pubkey == wallet.public_key {
                            pending_change += output.value.as_smallest_unit() as i64;
                        }
                    }
                }
            }

            println!("Available Balance:   {} WISP", available_balance);
            println!(
                "Pending Balance:     {} WISP",
                Amount((pending_change).max(0) as u64)
            );
            println!("Tracked UTXOs:       {}", utxos_guard.len());
            println!("Pending Txs:         {}", pending_tx_count);
        }
        Err(_) => {
            println!("⚠️ No wallet loaded.");
            println!("Node information is unavailable until a wallet is loaded.");
        }
    }

    pause();
    Ok(())
}

async fn delete_current_wallet_prompt(
    core: Arc<Core>,
    config_path: &PathBuf,
) -> Result<bool, anyhow::Error> {
    clear_terminal();
    display_heading();

    let current_wallet_name = match core.get_current_wallet().await {
        Ok(wallet) => {
            info!("Current wallet detected for deletion: {}", wallet.name);
            wallet.name
        }
        Err(e) => {
            println!("⚠️ No wallet is currently loaded. Cannot delete.");
            error!("Attempted to delete wallet when none loaded: {}", e);
            pause();
            return Ok(false);
        }
    };

    let password = match prompt_password(
        &format!(
            "Enter password for wallet '{}' to confirm deletion:",
            current_wallet_name
        ),
        false,
    ) {
        Ok(p) => p,
        Err(_) => {
            println!("\nPassword entry cancelled. Deletion aborted.");
            pause();
            return Ok(false);
        }
    };

    info!(
        "Password entered for deletion confirmation for wallet: {}",
        current_wallet_name
    );

    match core.decrypt_current_wallet_private_key(&password).await {
        Ok(_) => {
            info!(
                "Password for deletion confirmed for wallet: {}",
                current_wallet_name
            );
            let confirmation = Confirm::new(
                format!(
                    "Are you sure you want to delete wallet '{}'? This action is irreversible.",
                    current_wallet_name
                )
                .as_str(),
            )
            .with_default(false)
            .prompt()?;

            if confirmation {
                info!(
                    "Deletion confirmed by user for wallet: {}",
                    current_wallet_name
                );
                match core.delete_wallet(&current_wallet_name, config_path).await {
                    Ok(_) => {
                        println!("🗑️ Wallet '{}' deleted successfully.", current_wallet_name);
                        info!("Wallet '{}' deleted successfully.", current_wallet_name);
                        pause();
                        Ok(true)
                    }
                    Err(e) => {
                        error!("Failed to delete wallet '{}': {}", current_wallet_name, e);
                        println!("❌ Failed to delete wallet: {}", e);
                        pause();
                        Err(e)
                    }
                }
            } else {
                info!(
                    "Deletion cancelled by user for wallet: {}",
                    current_wallet_name
                );
                println!("Deletion cancelled.");
                pause();
                Ok(false)
            }
        }

        Err(e) => {
            error!(
                "Incorrect password for wallet '{}' deletion: {}",
                current_wallet_name, e
            );
            println!("⚠️ Incorrect password. Deletion cancelled.");
            pause();
            Ok(false)
        }
    }
}
