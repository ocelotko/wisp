use anyhow::Result;
use log::{debug, info, warn};
use std::sync::Arc;
use tokio::sync::RwLock;
use wisp_core::{
    blockchain::{AddBlockResult, Block, Blockchain},
    network::{ChainMessage, Message},
    sha256::Hash,
};

/// Handles the `NewBlock` message from a peer.
pub async fn handle_new_block(block: Block, blockchain: Arc<RwLock<Blockchain>>) -> Result<()> {
    let block_hash_for_log = block.id().unwrap_or_default();
    let block_index_for_log = block.index;
    let mut blockchain_lock = blockchain.write().await;
    let add_result = blockchain_lock.add_block(block.clone())?;

    drop(blockchain_lock);

    match add_result {
        AddBlockResult::Added => {
            info!(
                "Successfully added new block {} (index {}). Broadcasting to peers.",
                block_hash_for_log, block_index_for_log
            );
            super::mining::broadcast_block(block).await;
        }
        AddBlockResult::PotentialLongerForkDetected {
            common_ancestor_index,
            new_block_index,
            new_block_hash,
        } => {
            warn!(
                "Potential longer fork detected at block {} (index {}), ancestor {}. This should be handled internally by wisp-core.",
                new_block_hash, new_block_index, common_ancestor_index
            );
        }
        AddBlockResult::Orphaned => {
            info!(
                "Received orphan block {} (index {}), parent {} is unknown. It has been stored.",
                block_hash_for_log, block_index_for_log, block.previous_hash
            );
            tokio::spawn(async move {
                broadcast_request_for_block(block.previous_hash).await;
            });
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

/// Broadcasts a `FetchBlockByHash` message to all connected peers.
async fn broadcast_request_for_block(hash: Hash) {
    info!("Broadcasting request for missing block: {}", hash);
    let message = Message::Chain(ChainMessage::FetchBlockByHash(hash));

    for mut peer in crate::NODES.iter_mut() {
        let addr = peer.key().clone();
        let mut stream_lock = peer.value_mut().lock().await;
        if let Err(e) = message.send_async(&mut *stream_lock).await {
            warn!(
                "Failed to send block request to {}: {}. Connection may be stale.",
                addr, e
            );
        }
    }
}
