use anyhow::Result;
use log::{debug, info, warn};
use std::sync::Arc;
use tokio::{net::TcpStream, sync::RwLock};
use wisp_core::{
    blockchain::{AddBlockResult, Block, Blockchain},
    network::Message,
    signatures::PublicKey,
};

/// Handles a `FetchTemplate` request from a miner.
///
/// This function generates a new block template containing transactions from the mempool
/// and a coinbase transaction paying the reward to the miner's public key.
pub async fn handle_fetch_template(
    stream: &mut TcpStream,
    pubkey: PublicKey,
    blockchain: Arc<RwLock<Blockchain>>,
) -> Result<()> {
    let blockchain_lock = blockchain.read().await;
    let template = blockchain_lock.get_block_template_for_pubkey(&pubkey)?;
    debug!(
        "Sending new template for block #{} to miner {}",
        template.index,
        pubkey.fingerprint()
    );
    Message::Template(template).send_async(stream).await?;
    Ok(())
}

/// Handles a `SubmitTemplate` message from a miner.
///
/// This function receives a block that a miner claims to have solved. It performs
/// full validation by calling `add_block`. If the block is valid and accepted,
/// it sends a confirmation and broadcasts the new block to the network.
/// If rejected, it sends a rejection message.
pub async fn handle_submit_template(
    stream: &mut TcpStream,
    miner_pubkey: PublicKey, // We need the pubkey to generate the next template
    block: Block,
    blockchain: Arc<RwLock<Blockchain>>,
) -> Result<()> {
    let block_hash_for_log = block.id().unwrap_or_default();
    info!(
        "Received mined block submission {} (index {})",
        block_hash_for_log, block.index
    );

    // Acquire a write lock to attempt adding the block to the chain.
    let mut blockchain_lock = blockchain.write().await;

    // The `add_block` function handles all validation, fork choice, and state updates.
    let add_result = blockchain_lock.add_block(block.clone());

    match add_result {
        Ok(AddBlockResult::Added) => {
            info!(
                "Mined block {} accepted. Sending confirmation.",
                block_hash_for_log
            );

            let next_template = blockchain_lock.get_block_template_for_pubkey(&miner_pubkey)?;
            info!(
                "Generated next template for block #{} for the same miner.",
                next_template.index
            );

            // Send the new template back to the miner. This also serves as confirmation.
            Message::Template(next_template).send_async(stream).await?;
            drop(blockchain_lock); // Now we can drop the lock

            // Broadcast the newly mined block to all other peers. This can be a fire-and-forget task.
            tokio::spawn(async move {
                broadcast_block(block).await;
            });
        }
        Ok(AddBlockResult::Rejected(reason))
        | Ok(AddBlockResult::ShorterForkRejected(reason))
        | Ok(AddBlockResult::OrphanRejected(reason)) => {
            warn!("Mined block {} rejected: {}", block_hash_for_log, reason);
            drop(blockchain_lock);
            Message::BlockRejected(reason).send_async(stream).await?;
        }
        // A miner submitting a block should ideally not cause a reorg or an orphan situation,
        // as they should be building on the latest tip. However, due to network latency,
        // another block might arrive just as the miner submits theirs. In this case,
        // the node's `add_block` logic will correctly handle the fork. We can treat this
        // as a successful submission from the miner's perspective, as their block is now
        // known to the network, even if it doesn't become the main tip.
        Ok(AddBlockResult::PotentialLongerForkDetected { .. }) => {
            info!(
                "Mined block {} created a fork. This is acceptable. Sending confirmation.",
                block_hash_for_log
            );
            // In this case, we also generate a new template based on the new state.
            let next_template = blockchain_lock.get_block_template_for_pubkey(&miner_pubkey)?;
            Message::Template(next_template).send_async(stream).await?;
            drop(blockchain_lock);
            // The reorg logic will be handled by the node's regular block processing flow.
        }
        Ok(other) => {
            let reason = format!(
                "Submitted block resulted in an unexpected state and was not added: {:?}",
                other
            );
            warn!("{}", reason);
            drop(blockchain_lock);
            Message::BlockRejected(reason).send_async(stream).await?;
        }
        Err(e) => {
            let reason = format!("Error processing submitted block: {}", e);
            warn!("{}", reason);
            drop(blockchain_lock);
            Message::BlockRejected(reason).send_async(stream).await?;
        }
    }

    Ok(())
}

/// Broadcasts a block to all connected peers.
///
/// This function iterates through the global `NODES` map and sends a `NewBlock`
/// message to each peer. It's a "fire-and-forget" broadcast; it logs errors
/// but doesn't halt on individual send failures.
pub async fn broadcast_block(block: Block) {
    let message = Message::NewBlock(block);
    let mut peers_to_remove = Vec::new();

    for mut peer in crate::NODES.iter_mut() {
        let addr = peer.key().clone();
        if let Err(e) = message.send_async(peer.value_mut()).await {
            warn!(
                "Failed to broadcast block to {}: {}. Marking for removal.",
                addr, e
            );
            peers_to_remove.push(addr);
        }
    }

    // Clean up connections that failed during the broadcast.
    for addr in peers_to_remove {
        crate::NODES.remove(&addr);
    }
}
