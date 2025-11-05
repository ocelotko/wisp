use anyhow::Result;
use log::{debug, error, info, warn};
use std::sync::Arc;
use tokio::{sync::RwLock, time};
use wisp_core::{
    blockchain::{AddBlockResult, Block, Blockchain},
    network::Message,
    sha256::Hash,
};

/// Handles the `NewBlock` message from a peer.
///
/// This function attempts to add the received block to the blockchain. Based on the result,
/// it may trigger a chain reorganization, request missing parent blocks, or broadcast the
/// newly accepted block to other peers.
pub async fn handle_new_block(block: Block, blockchain: Arc<RwLock<Blockchain>>) -> Result<()> {
    let block_hash_for_log = block.id().unwrap_or_default();
    let block_index_for_log = block.index;

    // We need a write lock to modify the blockchain state.
    let mut blockchain_lock = blockchain.write().await;

    // Attempt to add the block to the chain. The `add_block` method contains all
    // the core validation and fork detection logic.
    let add_result = blockchain_lock.add_block(block.clone())?;

    // Drop the write lock as soon as possible to allow other tasks to proceed.
    // We will re-acquire it if necessary (e.g., for a reorg).
    drop(blockchain_lock);

    match add_result {
        AddBlockResult::Added => {
            info!(
                "Successfully added new block {} (index {}). Broadcasting to peers.",
                block_hash_for_log, block_index_for_log
            );
            // If the block was added, inform other peers.
            super::mining::broadcast_block(block).await;
        }
        AddBlockResult::PotentialLongerForkDetected {
            common_ancestor_index,
            new_block_index,
            new_block_hash,
        } => {
            warn!("Potential longer fork detected. New tip: {} (index {}). Fetching new chain segment from common ancestor at index {}.", new_block_hash, new_block_index, common_ancestor_index);

            // Fetch the missing blocks for the new fork.
            match fetch_chain_segment(new_block_hash, common_ancestor_index).await {
                Ok(new_chain_segment) => {
                    // Now perform the reorg with a write lock.
                    let mut blockchain_write_lock = blockchain.write().await;
                    if let Err(e) = blockchain_write_lock
                        .reorganize_chain(new_chain_segment, common_ancestor_index)
                    {
                        error!("Chain reorganization failed: {}", e);
                    }
                }
                Err(e) => {
                    error!("Failed to fetch chain segment for reorg: {}", e);
                }
            }
        }
        AddBlockResult::Orphaned => {
            info!(
                "Received orphan block {} (index {}), parent {} is unknown. It has been stored.",
                block_hash_for_log, block_index_for_log, block.previous_hash
            );
            broadcast_request_for_block(block.previous_hash).await;
        }
        AddBlockResult::Rejected(reason) => {
            warn!(
                "Rejected block {} (index {}): {}",
                block_hash_for_log, block_index_for_log, reason
            );
        }
        AddBlockResult::ShorterForkRejected(reason) => {
            debug!(
                "Rejected block on shorter fork {} (index {}): {}",
                block_hash_for_log, block_index_for_log, reason
            );
        }
        AddBlockResult::OrphanRejected(reason) => {
            warn!(
                "Rejected orphan block {} (index {}): {}",
                block_hash_for_log, block_index_for_log, reason
            );
        }
    }

    Ok(())
}

/// Fetches a segment of the blockchain from peers, starting from a given hash and walking backwards
/// until a block with an index less than or equal to `stop_at_index` is found.
async fn fetch_chain_segment(
    start_hash: Hash,
    stop_at_index: u64,
) -> Result<Vec<Block>, anyhow::Error> {
    let mut segment = Vec::new();
    let mut current_hash = start_hash;

    info!(
        "Fetching chain segment for reorg, starting from hash {} down to index {}.",
        start_hash, stop_at_index
    );

    // We need to fetch blocks until we have the full segment down to the common ancestor.
    loop {
        // First, check if we already have the block locally (it might be an orphan or already on disk).
        let local_block = {
            let blockchain = crate::BLOCKCHAIN.get().unwrap().read().await;
            blockchain.get_block_by_hash(&current_hash)?
        };

        let block = if let Some(b) = local_block {
            debug!("Found block {} locally for reorg segment.", b.id()?);
            b
        } else {
            // If not local, request it from the network.
            debug!(
                "Requesting block {} from network for reorg segment.",
                current_hash
            );
            let message = Message::FetchBlockByHash(current_hash);
            let mut found_block = None;

            // Iterate over peers to find one that has the block.
            for mut peer in crate::NODES.iter_mut() {
                if let Ok(_) = message.send_async(peer.value_mut()).await {
                    match time::timeout(
                        time::Duration::from_secs(5),
                        Message::receive_async(peer.value_mut()),
                    )
                    .await
                    {
                        Ok(Ok(Message::NewBlock(b))) => {
                            found_block = Some(b);
                            break; // Found it, no need to ask other peers.
                        }
                        _ => continue, // Timeout or wrong message, try next peer.
                    }
                }
            }

            found_block.ok_or_else(|| {
                anyhow::anyhow!("Failed to fetch block {} from any peer.", current_hash)
            })?
        };

        // Check if we've reached the common ancestor.
        if block.index <= stop_at_index {
            break;
        }

        segment.push(block.clone());
        current_hash = block.previous_hash;
    }

    // The blocks were added in reverse order (from tip to ancestor), so we must reverse the list.
    segment.reverse();
    Ok(segment)
}

/// Broadcasts a `FetchBlockByHash` message to all connected peers.
async fn broadcast_request_for_block(hash: Hash) {
    info!("Broadcasting request for missing block: {}", hash);
    let message = Message::FetchBlockByHash(hash);

    for mut peer in crate::NODES.iter_mut() {
        if let Err(e) = message.send_async(peer.value_mut()).await {
            warn!(
                "Failed to send block request to {}: {}. Connection may be stale.",
                peer.key(),
                e
            );
        }
    }
}
