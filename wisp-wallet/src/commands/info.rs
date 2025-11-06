use crate::utils::{
    clear_terminal, display_heading, display_heading_with_wallet, pause, prompt_password,
};
use crate::wallet::core::Core;
use anyhow::{Context, Result};
use inquire::{Confirm, Select};
use log::{error, info};
use std::path::PathBuf;
use std::sync::Arc;
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

            println!("\n--- Balance & State ---");
            let transactions_guard = core.transactions.read().await;
            let utxos_guard = core.utxos.read().await;

            // Confirmed balance is the sum of all UTXOs currently in the wallet's confirmed set.
            let confirmed_balance: Amount = utxos_guard.values().map(|output| output.value).sum();

            // Pending balance calculates the net change from all pending transactions.
            let mut pending_net_change: i128 = 0;
            let mut pending_tx_count = 0;

            for tx_info in transactions_guard.values() {
                if tx_info.status == TransactionStatus::Pending {
                    pending_tx_count += 1;
                    let tx = &tx_info.transaction;

                    // Subtract the value of inputs we owned that are being spent.
                    for input in &tx_info.transaction.inputs {
                        // An input is ours if it existed in our UTXO set before this tx.
                        if let Some(spent_utxo) = utxos_guard.get(&input.outpoint) {
                            pending_net_change -= spent_utxo.value.as_smallest_unit() as i128;
                        }
                    }

                    // Add the value of new outputs being sent to us (including our own change).
                    for output in &tx.outputs {
                        if output.pubkey == wallet.public_key {
                            pending_net_change += output.value.as_smallest_unit() as i128;
                        }
                    }
                }
            }

            let total_balance = Amount::from_smallest_unit(
                (confirmed_balance.as_smallest_unit() as i128 + pending_net_change).max(0) as u64,
            );

            println!(
                "Total Balance:       {} WISP (Confirmed + Pending)",
                total_balance
            );
            println!(
                "Available Balance:   {} WISP (Confirmed)",
                confirmed_balance
            );
            println!(
                "Pending Change:      {} WISP (Unconfirmed)",
                Amount::from_smallest_unit(pending_net_change.abs() as u64)
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
