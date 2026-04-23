mod funds;
mod info;
mod network;
mod security;
mod wallet_ops;

use crate::utils::{
    check_terminal_size, clear_terminal, display_heading, display_heading_with_wallet,
    exit_program, pause,
};
use crate::wallet::core::Core;
use anyhow::Result;
use funds::funds_management;
use info::settings_and_info;
use inquire::Select;
use log::{error, info};
use network::network_and_blockchain;
use security::security_and_backup;
use std::path::PathBuf;
use std::sync::Arc;

pub async fn run_wallet_ui(core: Arc<Core>, config_path: PathBuf) -> Result<(), anyhow::Error> {
    loop {
        clear_terminal();
        display_heading();

        if !check_terminal_size() {
            pause();
            continue;
        }

        let main_options = vec![
            "Open wallet",
            "Create new wallet",
            "Recover wallet from seed",
            "Recover wallet with private key",
            "Exit",
        ];

        let main_menu_selection = Select::new("Main Menu", main_options).prompt();

        match main_menu_selection.as_deref() {
            Ok("Open wallet") => {
                info!("Attempting to open wallet from main menu."); // Added log
                if let Err(e) = self::wallet_ops::open_wallet(Arc::clone(&core), &config_path).await
                {
                    error!("Failed to open wallet: {}", e);
                    println!("\nFailed to open wallet: {}", e);
                    pause();
                } else {
                    info!("Wallet loaded successfully. Proceeding to wallet management.");
                    if let Err(e) = wallet_management(Arc::clone(&core), &config_path).await {
                        error!("Wallet management exited with error: {}", e);
                        println!("\nWallet management error: {}", e);
                        pause();
                    }
                }
            }
            Ok("Create new wallet") => {
                info!("Attempting to create new wallet from main menu."); // Added log
                if let Err(e) =
                    self::wallet_ops::prompt_create_wallet(Arc::clone(&core), &config_path).await
                {
                    error!("Failed to create wallet: {}", e);
                    println!("\nFailed to create wallet: {}", e);
                    pause();
                } else {
                    info!("Wallet created successfully. Proceeding to wallet management.");
                    if let Err(e) = wallet_management(Arc::clone(&core), &config_path).await {
                        error!("Wallet management exited with error: {}", e);
                        println!("\nWallet management error: {}", e);
                        pause();
                    }
                }
            }
            Ok("Recover wallet with private key") => {
                info!("Attempting to recover wallet with private key from main menu.");
                if let Err(e) = self::wallet_ops::prompt_recover_wallet_with_key(
                    Arc::clone(&core),
                    &config_path,
                )
                .await
                {
                    error!("Failed to recover wallet: {}", e);
                    println!("\nFailed to recover wallet: {}", e);
                    pause();
                } else {
                    info!("Wallet recovered successfully. Proceeding to wallet management.");
                    if let Err(e) = wallet_management(Arc::clone(&core), &config_path).await {
                        error!("Wallet management exited with error: {}", e);
                    }
                }
            }
            Ok("Recover wallet from seed") => {
                info!("Attempting to recover wallet from seed from main menu.");
                if let Err(e) = self::wallet_ops::prompt_recover_wallet_with_seed(
                    Arc::clone(&core),
                    &config_path,
                )
                .await
                {
                    error!("Failed to recover wallet from seed: {}", e);
                    println!("\nFailed to recover wallet: {}", e);
                    pause();
                } else {
                    info!("Wallet recovered successfully. Proceeding to wallet management.");
                    if let Err(e) = wallet_management(Arc::clone(&core), &config_path).await {
                        error!("Wallet management exited with error: {}", e);
                    }
                }
            }
            Ok("Exit") => exit_program(),
            Err(e) => {
                error!("Main menu selection error: {}", e);
                println!("Invalid selection: {}", e);
                pause();
            }
            _ => { /* Should not happen with inquire::Select */ }
        }
    }
}

async fn wallet_management(core: Arc<Core>, config_path: &PathBuf) -> Result<()> {
    info!("Entered wallet_management function.");

    loop {
        info!("Start of wallet_management loop iteration.");
        core.fetch_wallet_state().await?;
        clear_terminal();
        let current_wallet_name = core
            .get_current_wallet()
            .await
            .ok()
            .map(|w| w.name.to_string());
        let balance_value = core.get_total_balance().await.ok();
        display_heading_with_wallet(current_wallet_name.as_deref(), balance_value);

        let wallet_options = vec![
            "Funds management",
            "Address & Watch-list management",
            "Network and blockchain",
            "Security and backup",
            "Wallet settings and info",
            "Back to main menu",
        ];
        let wallet_menu_selection = Select::new("Wallet Management", wallet_options).prompt()?; // Corrected: Added ? to unwrap the Result<String, Error>

        match wallet_menu_selection.as_ref() {
            "Funds management" => {
                if let Err(e) = funds_management(Arc::clone(&core), config_path).await {
                    error!("Funds management failed: {}", e);
                    println!("\nError: {}", e);
                    pause();
                }
            }

            "Address & Watch-list management" => {
                if let Err(e) = funds::address_management(Arc::clone(&core)).await {
                    error!("Address management failed: {}", e);
                    println!("\nError: {}", e);
                    pause();
                }
            }
            "Network and blockchain" => {
                if let Err(e) = network_and_blockchain(Arc::clone(&core), config_path).await {
                    error!("Network and blockchain failed: {}", e);
                    println!("\nError: {}", e);
                    pause();
                }
            }
            "Security and backup" => {
                if let Err(e) = security_and_backup(Arc::clone(&core), config_path).await {
                    error!("Security and backup failed: {}", e);
                    println!("\nError: {}", e);
                    pause();
                }
            }
            "Wallet settings and info" => {
                if let Err(e) = settings_and_info(Arc::clone(&core), config_path).await {
                    error!("Wallet settings and info failed: {}", e);
                    println!("\nError: {}", e);
                    pause();
                }
            }
            "Back to main menu" => return Ok(()),
            _ => {
                // Catches inquire::InquireError or unexpected string
                error!("Wallet management selection error: Unexpected selection or prompt error.");
                println!("Invalid selection or prompt error.");
                pause();
            }
        }
    }
}
