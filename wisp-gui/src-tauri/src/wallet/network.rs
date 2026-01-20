use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use log::{debug, info, warn};

use wisp_core::{
    blockchain::Block,
    network::{ChainMessage, Message, WalletMessage},
};

use crate::wallet::{account::get_current_wallet, core::Core};

pub async fn set_default_node(
    core: &Core,
    new_node_address: &str,
    config_path: &PathBuf,
) -> Result<()> {
    let mut config_guard = core.config.lock().await;
    config_guard.default_node = new_node_address.to_string();

    let mut stream_lock = core.connected_node_stream.lock().await;
    *stream_lock = None;

    core.save_config(config_path, &*config_guard).await?;

    info!("Default node address updated to: {}", new_node_address);
    Ok(())
}

pub async fn fetch_peers_from_node(core: &Core) -> Result<Vec<String>> {
    let mut stream_guard = core.get_connected_stream().await?;
    let stream_ref = stream_guard
        .as_mut()
        .expect("Expected an active TCP stream after connection attempt");
    let response_timeout = core.get_node_response_timeout().await;

    let msg = Message::P2P(wisp_core::network::P2PMessage::DiscoverNodes);
    if let Err(e) = msg.send_async(stream_ref).await {
        *stream_guard = None;
        return Err(anyhow!("Failed to send DiscoverNodes message: {}", e));
    }

    let response =
        match tokio::time::timeout(response_timeout, Message::receive_async(stream_ref)).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(e)) => {
                *stream_guard = None;
                return Err(anyhow!("Failed to receive NodeList response: {}", e));
            }
            Err(_) => {
                *stream_guard = None;
                return Err(anyhow!("Timeout waiting for NodeList response"));
            }
        };

    match response {
        Message::P2P(wisp_core::network::P2PMessage::NodeList(peers)) => Ok(peers),
        other => {
            *stream_guard = None;
            Err(anyhow!(
                "Unexpected response for DiscoverNodes: {:?}",
                other
            ))
        }
    }
}

pub async fn fetch_wallet_state(core: &Core) -> Result<()> {
    let current_wallet = get_current_wallet(core).await?;
    let wallet_public_key = current_wallet.public_key.clone();
    info!(
        "Fetching wallet state for public key: {}",
        wallet_public_key.fingerprint()
    );

    debug!("fetch_wallet_state: Attempting to get connected stream.");
    let mut stream_guard = core.get_connected_stream().await?;
    debug!("fetch_wallet_state: Connected stream obtained.");
    let stream_ref = stream_guard
        .as_mut()
        .expect("Expected an active TCP stream after connection attempt");
    let response_timeout = core.get_node_response_timeout().await;

    let fetch_state_msg =
        Message::Wallet(WalletMessage::FetchWalletState(wallet_public_key.clone()));
    if let Err(e) = fetch_state_msg.send_async(stream_ref).await {
        *stream_guard = None;
        return Err(anyhow!("Failed to send FetchWalletState message: {}", e));
    }
    debug!("fetch_wallet_state: Waiting for WalletState response.");

    let state_response =
        match tokio::time::timeout(response_timeout, Message::receive_async(stream_ref)).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(e)) => {
                *stream_guard = None;
                return Err(anyhow!("Failed to receive WalletState response: {}", e));
            }
            Err(_) => {
                *stream_guard = None;
                return Err(anyhow!("Timeout waiting for WalletState response"));
            }
        };

    match state_response {
        Message::Wallet(WalletMessage::WalletState(snapshot)) => {
            debug!("fetch_wallet_state: Received WalletState snapshot.");
            let mut transactions_guard = core.transactions.write().await;
            transactions_guard.clear();
            for tx_info in snapshot.transactions {
                transactions_guard.insert(tx_info.transaction.txid()?, tx_info);
            }
            info!(
                "Updated wallet with {} total transactions.",
                transactions_guard.len()
            );

            let mut utxos_guard = core.utxos.write().await;
            utxos_guard.clear();
            for (outpoint, output) in snapshot.utxos {
                utxos_guard.insert(outpoint, output);
            }

            info!("Fetched {} available UTXOs for wallet.", utxos_guard.len());
        }
        other => {
            *stream_guard = None;
            return Err(anyhow!(
                "Unexpected response for FetchWalletState: {:?}",
                other
            ));
        }
    }

    info!("fetch_wallet_state: Wallet state fetch completed successfully.");
    Ok(())
}

pub async fn fetch_incremental_state(core: &Core) -> Result<()> {
    let current_wallet = get_current_wallet(core).await?;

    // 1. Determine local high-water mark
    let last_height = {
        let txs = core.transactions.read().await;
        txs.values()
            .filter_map(|info| info.block_index)
            .max()
            .unwrap_or(0)
    };

    let mut stream_guard = core.get_connected_stream().await?;
    let stream_ref = stream_guard.as_mut().unwrap();

    // 2. Request only what's new
    let msg = Message::Wallet(WalletMessage::FetchWalletUpdates {
        public_key: current_wallet.public_key.clone(),
        since_height: last_height,
    });
    msg.send_async(stream_ref).await?;

    // 3. Receive and MERGE
    if let Message::Wallet(WalletMessage::WalletUpdates(updates)) =
        Message::receive_async(stream_ref).await?
    {
        // Merge Transactions
        {
            let mut tx_lock = core.transactions.write().await;
            for tx_info in updates.transactions {
                // This overwrites Pending with Confirmed if the ID matches
                tx_lock.insert(tx_info.transaction.txid()?, tx_info);
            }
        }

        // Merge UTXOs
        {
            let mut utxo_lock = core.utxos.write().await;
            for (outpoint, output) in updates.utxos {
                utxo_lock.insert(outpoint, output);
            }
        }
    }
    Ok(())
}

pub async fn get_block_info(core: &Core, index: u64) -> Result<Option<Block>> {
    let mut stream_guard = core.get_connected_stream().await?;
    let stream_ref = stream_guard
        .as_mut()
        .expect("Expected an active TCP stream after connection attempt");
    let response_timeout = core.get_node_response_timeout().await;

    let msg = Message::Chain(ChainMessage::FetchBlockInfo(index));
    if let Err(e) = msg.send_async(stream_ref).await {
        *stream_guard = None;
        return Err(anyhow!("Failed to send FetchBlockInfo message: {}", e));
    }

    let response =
        match tokio::time::timeout(response_timeout, Message::receive_async(stream_ref)).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(e)) => {
                *stream_guard = None;
                return Err(anyhow!("Failed to receive FetchBlockInfo response: {}", e));
            }
            Err(_) => {
                *stream_guard = None;
                return Err(anyhow!("Timeout waiting for FetchBlockInfo response"));
            }
        };

    match response {
        Message::Chain(ChainMessage::BlockInfo(block)) => Ok(block),
        other => {
            *stream_guard = None;
            Err(anyhow!(
                "Unexpected response for FetchBlockInfo: {:?}",
                other
            ))
        }
    }
}

pub async fn get_latest_block(core: &Core) -> Result<Option<(Block, u64)>> {
    let mut stream_guard = core.get_connected_stream().await?;
    let stream_ref = stream_guard
        .as_mut()
        .expect("Expected an active TCP stream after connection attempt");
    let response_timeout = core.get_node_response_timeout().await;

    let msg = Message::Chain(ChainMessage::FetchLatestBlock);
    if let Err(e) = msg.send_async(stream_ref).await {
        *stream_guard = None;
        return Err(anyhow!("Failed to send FetchLatestBlock message: {}", e));
    }

    let response =
        match tokio::time::timeout(response_timeout, Message::receive_async(stream_ref)).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(e)) => {
                *stream_guard = None;
                return Err(anyhow!(
                    "Failed to receive FetchLatestBlock response: {}",
                    e
                ));
            }
            Err(_) => {
                *stream_guard = None;
                return Err(anyhow!("Timeout waiting for FetchLatestBlock response"));
            }
        };

    match response {
        Message::Chain(ChainMessage::LatestBlock(block_and_height)) => Ok(block_and_height),
        other => {
            *stream_guard = None;
            Err(anyhow!(
                "Unexpected response for FetchLatestBlock: {:?}",
                other
            ))
        }
    }
}

pub async fn start_background_sync(core: Arc<Core>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));

        loop {
            interval.tick().await;

            let wallet_loaded = {
                let config_guard = core.config.lock().await;
                if let Some(name) = &config_guard.current_wallet_name {
                    let wallets_guard = core.wallets.lock().await;
                    wallets_guard.iter().any(|w| w.name == *name)
                } else {
                    false
                }
            };

            if wallet_loaded {
                debug!("Background sync: Fetching wallet state...");
                if let Err(e) = fetch_wallet_state(&core).await {
                    warn!("Background sync failed: {}", e);
                } else {
                    info!("Background sync completed successfully.");
                }
            } else {
                debug!("Background sync: No wallet loaded, skipping fetch.");
            }
        }
    });
}
