use crate::utils::{clear_terminal, display_heading_with_wallet, pause};
use crate::wallet::core::Core;
use anyhow::{Context, Result};
use inquire::{Select, Text};
use log::error;
use std::sync::Arc;

pub async fn network_and_blockchain(core: Arc<Core>) -> Result<(), anyhow::Error> {
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
            "Connect to node" => connect_node(&core).await?,
            "List connected peers" => list_peers(&core).await,
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
            println!("⚠️ Could not retrieve the latest block from the node.");
        }
        Err(e) => {
            error!("Failed to get latest block: {}", e);
            println!("❌ Failed to get latest block: {}", e);
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
            println!("❌ Failed to get block information: {}", e);
        }
    }

    pause();
    Ok(())
}

async fn connect_node(core: &Core) -> Result<(), anyhow::Error> {
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

    println!("Attempting to connect to default node...");
    match core.get_connected_stream().await {
        Ok(_) => {
            println!("✅ Successfully connected to the default node.");
        }
        Err(e) => {
            error!("Failed to connect to node: {}", e);
            println!("❌ Failed to connect to node: {}", e);
        }
    }
    pause();
    Ok(())
}

async fn list_peers(core: &Core) {
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
    println!("Discovered Peers:");
    println!("------------------");

    let discovered_nodes_guard = core.discovered_nodes.lock().await;
    if discovered_nodes_guard.is_empty() {
        println!("No peers have been discovered yet.");
    } else {
        for (address, last_seen) in discovered_nodes_guard.iter() {
            match last_seen {
                Some(duration) => {
                    let total_secs = duration.as_secs();
                    let mins = total_secs / 60;
                    let secs = total_secs % 60;
                    println!("Address: {}, Last Seen: {}m {}s ago", address, mins, secs);
                }
                None => println!("Address: {}, Status: Unknown/Inactive", address),
            }
        }
    }
    println!("------------------");
    pause();
}
