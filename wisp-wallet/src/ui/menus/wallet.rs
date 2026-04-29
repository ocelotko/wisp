use crate::engine::session::Core;
use crate::ui::clear_terminal;
use crate::ui::menus::funds::{self, funds_management_loop};
use crate::ui::menus::info::settings_and_info_loop;
use crate::ui::menus::network::network_and_blockchain_loop;
use crate::ui::menus::security::security_and_backup_loop;
use crate::ui::views::layout::{display_heading_with_wallet, pause};
use anyhow::Result;
use inquire::Select;
use log::{error, info, warn};
use std::path::PathBuf;
use std::sync::Arc;

pub async fn wallet_management_loop(core: Arc<Core>, config_path: &PathBuf) -> Result<()> {
    info!("Entered wallet_management function.");

    loop {
        info!("Start of wallet_management loop iteration.");
        if let Err(e) = core.fetch_wallet_state().await {
            warn!(
                "Failed to fetch wallet state in wallet management loop: {}",
                e
            );
        }
        clear_terminal();
        let summary = core.get_wallet_summary().await.ok();
        let wallet_name = core.get_current_wallet().await.ok().map(|w| w.name);
        display_heading_with_wallet(wallet_name.as_deref(), summary.map(|s| s.total_balance));

        let wallet_options = vec![
            "Funds management",
            "Address & Watch-list management",
            "Network and blockchain",
            "Security and backup",
            "Wallet settings and info",
            "Back to main menu",
        ];
        let wallet_menu_selection = Select::new("Wallet Management", wallet_options).prompt()?;

        match wallet_menu_selection.as_ref() {
            "Funds management" => {
                if let Err(e) = funds_management_loop(Arc::clone(&core), config_path).await {
                    error!("Funds management failed: {}", e);
                    println!("\nError: {}", e);
                    pause();
                }
            }

            "Address & Watch-list management" => {
                if let Err(e) = funds::address_management_loop(Arc::clone(&core)).await {
                    error!("Address management failed: {}", e);
                    println!("\nError: {}", e);
                    pause();
                }
            }
            "Network and blockchain" => {
                if let Err(e) = network_and_blockchain_loop(Arc::clone(&core), config_path).await {
                    error!("Network and blockchain failed: {}", e);
                    println!("\nError: {}", e);
                    pause();
                }
            }
            "Security and backup" => {
                if let Err(e) = security_and_backup_loop(Arc::clone(&core), config_path).await {
                    error!("Security and backup failed: {}", e);
                    println!("\nError: {}", e);
                    pause();
                }
            }
            "Wallet settings and info" => {
                if let Err(e) = settings_and_info_loop(Arc::clone(&core), config_path).await {
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
