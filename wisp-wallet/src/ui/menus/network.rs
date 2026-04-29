use anyhow::{Context, Result};
use inquire::{Confirm, Select, Text};
use log::error;
use std::path::PathBuf;
use std::sync::Arc;

use crate::engine::session::Core;
use crate::ui::clear_terminal;
use crate::ui::views::layout::{display_heading_with_wallet, pause};

pub async fn network_and_blockchain_loop(
    core: Arc<Core>,
    config_path: &PathBuf,
) -> Result<(), anyhow::Error> {
    loop {
        clear_terminal();
        let summary = core.get_wallet_summary().await.ok();
        let wallet = core.get_current_wallet().await?;
        display_heading_with_wallet(Some(&wallet.name), summary.map(|s| s.total_balance));

        let blockchain_options = vec![
            "Get latest block information",
            "Get specific block information",
            "Connect to node",
            "List connected peers",
            "Back to wallet menu",
        ];
        let blockchain_menu_selection =
            Select::new("Blockchain Interaction", blockchain_options).prompt()?;

        match blockchain_menu_selection.as_ref() {
            "Get latest block information" => latest_block_view(&core).await?,
            "Get specific block information" => block_info_prompt(&core).await?,
            "Connect to node" => connect_node_prompt(&core, config_path).await?,
            "List connected peers" => list_peers_view(&core).await?,
            "Back to wallet menu" => return Ok(()),
            _ => {
                error!("Blockchain interaction menu selection error: Invalid selection or prompt error.");
                println!("Invalid selection or prompt error.");
                pause();
            }
        }
    }
}

async fn latest_block_view(core: &Core) -> Result<(), anyhow::Error> {
    clear_terminal();
    let summary = core.get_wallet_summary().await.ok();
    let wallet = core.get_current_wallet().await?;
    display_heading_with_wallet(Some(&wallet.name), summary.map(|s| s.total_balance));
    println!("Getting latest block from the node...");

    match core.get_latest_block().await {
        Ok(Some((block, height))) => {
            println!("\n--- Latest Block Information ---");
            println!("Height: {}", height);
            println!("Index (from block header): {}", block.index);
            println!("Timestamp: {}", block.header.timestamp);
            println!("Nonce: {}", block.header.nonce);
            println!("Previous Hash: {}", block.header.previous_hash);
            println!("Merkle Root: {:?}", block.header.merkle_root);
            println!("Target: {}", block.header.target);
            println!("Number of Transactions: {}", block.transactions.len());
            println!("Transactions:");
            if block.transactions.is_empty() {
                println!("  (No transactions)");
            } else {
                for tx in &block.transactions {
                    println!(
                        "  Hash: {}",
                        tx.txid()
                            .ok()
                            .map_or("<unhashable>".to_string(), |hash| hash.to_string())
                    );
                    if tx.inputs.is_empty() {
                        println!("    (Coinbase transaction)");
                    }
                }
            }
            println!("-------------------------");
        }
        Ok(None) => {
            println!("Could not retrieve the latest block from the node.");
        }
        Err(e) => {
            error!("Failed to get latest block: {}", e);
            println!("Failed to get latest block: {}", e);
        }
    }

    pause();
    Ok(())
}

async fn block_info_prompt(core: &Core) -> Result<(), anyhow::Error> {
    clear_terminal();
    let summary = core.get_wallet_summary().await.ok();
    let wallet = core.get_current_wallet().await?;
    display_heading_with_wallet(Some(&wallet.name), summary.map(|s| s.total_balance));

    let index_str = Text::new("Enter the index of the block you want to view:").prompt()?;
    let index = index_str
        .parse::<u64>()
        .context("Invalid block index entered, must be a number")?;

    match core.get_block_info(index).await {
        Ok(Some(block)) => {
            println!("\n--- Block Information ---");
            println!("Index: {}", block.index);
            println!("Timestamp: {}", block.header.timestamp);
            println!("Nonce: {}", block.header.nonce);
            println!("Previous Hash: {}", block.header.previous_hash);
            println!("Merkle Root: {:?}", block.header.merkle_root);
            println!("Target: {}", block.header.target);
            println!("Number of Transactions: {}", block.transactions.len());
            println!("Transactions:");
            if block.transactions.is_empty() {
                println!("  (No transactions)");
            } else {
                for tx in &block.transactions {
                    println!(
                        "  Hash: {}",
                        tx.txid()
                            .ok()
                            .map_or("<unhashable>".to_string(), |hash| hash.to_string())
                    );
                    if tx.inputs.is_empty() {
                        println!("    (Coinbase transaction)");
                    }
                }
            }
            println!("-------------------------");
        }
        Ok(None) => {
            println!("Block with index {} not found on the node.", index);
        }
        Err(e) => {
            error!("Failed to get block info: {}", e);
            println!("Failed to get block information: {}", e);
        }
    }

    pause();
    Ok(())
}

async fn connect_node_prompt(core: &Core, config_path: &PathBuf) -> Result<(), anyhow::Error> {
    clear_terminal();
    let summary = core.get_wallet_summary().await.ok();
    let wallet = core.get_current_wallet().await?;
    display_heading_with_wallet(Some(&wallet.name), summary.map(|s| s.total_balance));

    let current_node = core.get_default_node_address().await;
    println!("Current default node: {}", current_node);

    let change_node = Confirm::new("Do you want to change the default node?")
        .with_default(false)
        .prompt()?;

    if change_node {
        let new_node_addr = Text::new("Enter new node address (e.g., 127.0.0.1:9000):").prompt()?;

        println!("Attempting to connect to new node: {}", new_node_addr);
        core.set_default_node(&new_node_addr, config_path).await?;

        match core.get_connected_stream().await {
            Ok(_) => {
                println!("Successfully connected to new node and set it as default.");
            }
            Err(e) => {
                error!("Failed to connect to new node: {}", e);
                println!(
                    "\nFailed to connect to new node: {}. Reverting to previous default.",
                    e
                );
                // Attempt to revert to the previous default node if connection fails
                core.set_default_node(&current_node, config_path).await?;
            }
        }
    } else {
        println!("\nAttempting to connect to default node...");
        match core.get_connected_stream().await {
            Ok(_) => {
                println!("Successfully connected to the default node.");
            }
            Err(e) => {
                error!("Failed to connect to node: {}", e);
                println!("Failed to connect to node: {}", e);
            }
        }
    }

    pause();
    Ok(())
}

async fn list_peers_view(core: &Core) -> Result<()> {
    clear_terminal();
    let summary = core.get_wallet_summary().await.ok();
    let wallet = core.get_current_wallet().await?;
    display_heading_with_wallet(Some(&wallet.name), summary.map(|s| s.total_balance));
    println!("Fetching peer list from connected node...");
    println!("------------------");

    match core.fetch_peers_from_node().await {
        Ok(peers) => {
            if peers.is_empty() {
                println!("Node reported no other connected peers.");
            } else {
                println!("Node is connected to the following peers:");
                for peer_addr in peers {
                    println!("- {}", peer_addr);
                }
            }
        }
        Err(e) => {
            error!("Failed to list peers: {}", e);
            println!("\nError fetching peer list: {}", e);
        }
    }

    println!("------------------");
    pause();
    Ok(())
}
