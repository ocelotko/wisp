use crate::utils::{clear_terminal, display_heading_with_wallet, pause};
use crate::wallet::core::Core;
use anyhow::{Context, Result};
use inquire::{Select, Text};
use log::error;
use std::{path::PathBuf, sync::Arc};

pub async fn network_and_blockchain(
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
            "Get latest block information" => get_latest_block_prompt(&core).await?,
            "Get specific block information" => get_block_info_prompt(&core).await?,
            "Connect to node" => connect_node(&core, config_path).await?,
            "List connected peers" => list_peers(&core).await?,
            "Back to wallet menu" => return Ok(()),
            _ => {
                error!("Blockchain interaction menu selection error: Invalid selection or prompt error.");
                println!("Invalid selection or prompt error.");
                pause();
            }
        }
    }
}

async fn get_latest_block_prompt(core: &Core) -> Result<(), anyhow::Error> {
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
    println!("Getting latest block from the node...");

    match core.get_latest_block().await {
        Ok(Some((block, height))) => {
            println!("\n--- Latest Block Information ---");
            println!("Height: {}", height);
            println!("Index (from block header): {}", block.index);
            println!("Timestamp: {}", block.timestamp);
            println!("Nonce: {}", block.nonce);
            println!("Previous Hash: {}", block.previous_hash);
            println!("Merkle Root: {:?}", block.merkle_root);
            println!("Target: {}", block.target);
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

async fn get_block_info_prompt(core: &Core) -> Result<(), anyhow::Error> {
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

    let index_str = Text::new("Enter the index of the block you want to view:").prompt()?;
    let index = index_str
        .parse::<u64>()
        .context("Invalid block index entered, must be a number")?;

    match core.get_block_info(index).await {
        Ok(Some(block)) => {
            println!("\n--- Block Information ---");
            println!("Index: {}", block.index);
            println!("Timestamp: {}", block.timestamp);
            println!("Nonce: {}", block.nonce);
            println!("Previous Hash: {}", block.previous_hash);
            println!("Merkle Root: {:?}", block.merkle_root);
            println!("Target: {}", block.target);
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

async fn connect_node(core: &Core, config_path: &PathBuf) -> Result<(), anyhow::Error> {
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

    let current_node = {
        let config = core.config.lock().await;
        config.default_node.clone()
    };
    println!("Current default node: {}", current_node);

    let change_node = inquire::Confirm::new("Do you want to change the default node?")
        .with_default(false)
        .prompt()?;

    if change_node {
        let new_node_addr =
            inquire::Text::new("Enter new node address (e.g., 127.0.0.1:9000):").prompt()?;

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

async fn list_peers(core: &Core) -> Result<()> {
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
