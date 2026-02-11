use anyhow::Result;
use log::{debug, info, warn};
use std::sync::Arc;
use tokio::{
    net::TcpStream,
    sync::{Mutex as AsyncMutex, RwLock},
};
use wisp_core::{
    blockchain::{AddBlockResult, Block, Blockchain},
    network::{ChainMessage, Message},
    sha256::Hash,
};

/// Handles the `NewBlock` message from a peer.
pub async fn handle_new_block(
    block: Block,
    blockchain: Arc<RwLock<Blockchain>>,
    sender_stream_arc: Arc<AsyncMutex<TcpStream>>,
) -> Result<()> {
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
            broadcast_new_block_from_peer(block, sender_stream_arc).await;
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
                block_hash_for_log, block_index_for_log, block.header.previous_hash
            );
            tokio::spawn(async move {
                broadcast_request_for_block(block.header.previous_hash).await;
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

/// Broadcasts a new valid block to all peers except the one it was received from.
async fn broadcast_new_block_from_peer(
    block: Block,
    original_sender_arc: Arc<AsyncMutex<TcpStream>>,
) {
    let message = Message::Chain(ChainMessage::NewBlock(block));
    let filter = crate::connection::BroadcastFilter::AllExcept(original_sender_arc);
    crate::connection::broadcast(&message, filter, "new block").await;
}

/// Broadcasts a `FetchBlockByHash` message to all connected peers.
async fn broadcast_request_for_block(hash: Hash) {
    info!("Broadcasting request for missing block: {}", hash);
    let message = Message::Chain(ChainMessage::FetchBlockByHash(hash));
    crate::connection::broadcast(
        &message,
        crate::connection::BroadcastFilter::All,
        "block request",
    )
    .await;
}
