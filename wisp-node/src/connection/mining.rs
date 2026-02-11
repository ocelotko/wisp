use anyhow::Result;
use log::{debug, info, warn};
use std::sync::Arc;
use tokio::{net::TcpStream, sync::RwLock};
use wisp_core::{
    blockchain::{AddBlockResult, Block, Blockchain},
    network::{ChainMessage, Message, MiningMessage},
    signatures::PublicKey,
};

/// Handles a `FetchTemplate` request from a miner.
pub async fn handle_fetch_template(
    stream: &mut TcpStream,
    pubkey: PublicKey,
    coinbase_message: Option<String>,
    blockchain: Arc<RwLock<Blockchain>>,
) -> Result<()> {
    let blockchain_lock = blockchain.read().await;
    let template = blockchain_lock.get_block_template(&pubkey, coinbase_message.as_deref())?;
    debug!(
        "Sending new template for block #{} to miner {}",
        template.index,
        pubkey.fingerprint()
    );
    Message::Mining(MiningMessage::Template(template))
        .send_async(stream)
        .await?;
    Ok(())
}

/// Handles a `SubmitTemplate` message from a miner.
pub async fn handle_submit_template(
    stream: &mut TcpStream,
    miner_pubkey: PublicKey,
    block: Block,
    coinbase_message: Option<String>,
    blockchain: Arc<RwLock<Blockchain>>,
) -> Result<()> {
    let block_hash_for_log = block.id().unwrap_or_default();
    info!(
        "Received mined block submission {} (index {})",
        block_hash_for_log, block.index
    );

    let mut blockchain_lock = blockchain.write().await;
    let add_result = blockchain_lock.add_block(block.clone());

    match add_result {
        Ok(AddBlockResult::Added) => {
            info!(
                "Mined block {} accepted. Sending confirmation.",
                block_hash_for_log
            );

            let next_template =
                blockchain_lock.get_block_template(&miner_pubkey, coinbase_message.as_deref())?;
            info!(
                "Generated next template for block #{} for the same miner.",
                next_template.index
            );

            Message::Mining(MiningMessage::Template(next_template))
                .send_async(stream)
                .await?;
            drop(blockchain_lock);

            tokio::spawn(async move {
                broadcast_block(block).await;
            });
        }
        Ok(AddBlockResult::Rejected(reason))
        | Ok(AddBlockResult::ShorterForkRejected(reason))
        | Ok(AddBlockResult::OrphanRejected(reason)) => {
            warn!("Mined block {} rejected: {}", block_hash_for_log, reason);
            drop(blockchain_lock);
            Message::Mining(MiningMessage::BlockRejected(reason))
                .send_async(stream)
                .await?;
        }
        Ok(other) => {
            let reason = format!(
                "Submitted block resulted in an unexpected state and was not added: {:?}",
                other
            );
            warn!("{}", reason);
            drop(blockchain_lock);
            Message::Mining(MiningMessage::BlockRejected(reason))
                .send_async(stream)
                .await?;
        }
        Err(e) => {
            let reason = format!("Error processing submitted block: {}", e);
            warn!("{}", reason);
            drop(blockchain_lock);
            Message::Mining(MiningMessage::BlockRejected(reason))
                .send_async(stream)
                .await?;
        }
    }

    Ok(())
}

/// Broadcasts a block to all connected peers.
pub async fn broadcast_block(block: Block) {
    let block_id = block.id().unwrap_or_default();
    info!("Broadcasting new block {} to all peers.", block_id);
    let message = Message::Chain(ChainMessage::NewBlock(block));
    super::broadcast(&message, super::BroadcastFilter::All, "block").await;
}
