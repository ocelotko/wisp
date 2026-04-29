use anyhow::Result;
use inquire::{Confirm, Select};
use std::path::PathBuf;
use std::sync::Arc;

use crate::engine::session::Core;
use crate::ui::clear_terminal;
use crate::ui::views::layout::{display_heading_with_wallet, pause};
use crate::utils::prompt_password;

pub async fn settings_and_info_loop(core: Arc<Core>, config_path: &PathBuf) -> Result<()> {
    loop {
        clear_terminal();
        let summary = core.get_wallet_summary().await.ok();
        let wallet = core.get_current_wallet().await?;
        display_heading_with_wallet(
            Some(&wallet.name),
            summary.as_ref().map(|s| s.total_balance),
        );

        let options = vec![
            "View wallet & node info",
            "Delete current wallet",
            "Back to wallet menu",
        ];
        let selection = Select::new("Wallet Settings and Info", options).prompt()?;

        match selection {
            "View wallet & node info" => {
                wallet_info_view(&core).await?;
            }
            "Delete current wallet" => {
                if delete_wallet_prompt(core.clone(), config_path).await? {
                    return Ok(());
                }
            }
            "Back to wallet menu" => return Ok(()),
            _ => {}
        }
    }
}

async fn wallet_info_view(core: &Core) -> Result<()> {
    clear_terminal();
    let summary = core.get_wallet_summary().await?;
    let wallet = core.get_current_wallet().await?;

    println!("\n--- Wallet Details ---");
    println!("{:<20} {}", "Name:", wallet.name);
    println!("{:<20} {}", "Public Key:", wallet.public_key.fingerprint());

    let addresses = core.get_receive_addresses(&wallet)?;
    if let Some((label, addr)) = addresses.iter().find(|(l, _)| l.contains("Aurora")) {
        println!("{:<20} {}", label, addr);
    }

    println!("\n--- Network Status ---");
    let node_addr = core.get_default_node_address().await;
    println!("{:<20} {}", "Connected Node:", node_addr);

    println!("\n--- Balance & State ---");
    println!(
        "{:<20} {} WISP",
        "Total Balance:",
        summary.total_balance.to_string_wisp()
    );
    println!(
        "{:<20} {} WISP",
        "Confirmed:",
        summary.confirmed_balance.to_string_wisp()
    );

    let pending_status = if summary.pending_net_change >= 0 {
        "+"
    } else {
        ""
    };
    println!(
        "{:<20} {}{} smallest units",
        "Pending Change:", pending_status, summary.pending_net_change
    );

    println!("{:<20} {}", "UTXO Count:", summary.utxo_count);
    println!("{:<20} {}", "Pending Txs:", summary.pending_tx_count);

    pause();
    Ok(())
}

async fn delete_wallet_prompt(core: Arc<Core>, config_path: &PathBuf) -> Result<bool> {
    let wallet = core.get_current_wallet().await?;
    let name = wallet.name.clone();

    println!("\nWARNING: Deleting a wallet is irreversible.");
    println!("Make sure you have your seed phrase backed up before proceeding.");

    let password = prompt_password(
        &format!("Enter password for '{}' to confirm deletion:", name),
        false,
    )?;

    if core.decrypt_current_private_key(&password).await.is_err() {
        println!("Incorrect password. Deletion aborted.");
        pause();
        return Ok(false);
    }

    let confirm = Confirm::new(&format!(
        "Are you absolutely sure you want to delete '{}'?",
        name
    ))
    .with_default(false)
    .prompt()?;

    if confirm {
        core.delete_wallet(&name, config_path).await?;
        println!("Wallet '{}' has been deleted.", name);
        pause();
        Ok(true)
    } else {
        println!("Deletion cancelled.");
        pause();
        Ok(false)
    }
}
