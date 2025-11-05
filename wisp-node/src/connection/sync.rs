use anyhow::Result;
use log::{debug, warn};
use std::sync::Arc;
use tokio::{net::TcpStream, sync::RwLock};
use wisp_core::{
    blockchain::{Block, Blockchain},
    network::Message,
    sha256::Hash,
};

/// Handles a `FetchBlock` request from a peer by sending back the requested block if it exists.
pub async fn handle_fetch_block(
    stream: &mut TcpStream,
    index: u64,
    blockchain: Arc<RwLock<Blockchain>>,
) -> Result<()> {
    debug!("Handling FetchBlock request for index {}", index);
    let blockchain_lock = blockchain.read().await;
    let block = blockchain_lock.get_block_by_index(index)?;
    drop(blockchain_lock);

    let response = if let Some(b) = block {
        // The convention for a direct block request is to respond with a `NewBlock` message.
        // This is what the `download_blockchain` utility function expects.
        Message::NewBlock(b)
    } else {
        // If the block is not found, we respond with `BlockInfo(None)` to explicitly
        // signal that the block is missing, which is better than a timeout.
        warn!(
            "Block at index {} not found, sending negative response.",
            index
        );
        Message::BlockInfo(None)
    };

    response.send_async(stream).await?;
    Ok(())
}

/// Handles a `FetchBlockByHash` request from a peer by sending back the requested block if it exists.
pub async fn handle_fetch_block_by_hash(
    stream: &mut TcpStream,
    hash: Hash,
    blockchain: Arc<RwLock<Blockchain>>,
) -> Result<()> {
    debug!("Handling FetchBlockByHash request for hash {}", hash);
    let blockchain_lock = blockchain.read().await;
    let block = blockchain_lock.get_block_by_hash(&hash)?;
    drop(blockchain_lock);

    let response = if let Some(b) = block {
        Message::NewBlock(b)
    } else {
        warn!(
            "Block with hash {} not found, sending negative response.",
            hash
        );
        Message::BlockInfo(None)
    };

    response.send_async(stream).await?;
    Ok(())
}

/// Handles a `FetchLatestBlock` request from a peer by sending back the current tip of the chain.
pub async fn handle_fetch_latest_block(
    stream: &mut TcpStream,
    blockchain: Arc<RwLock<Blockchain>>,
) -> Result<()> {
    debug!("Handling FetchLatestBlock request.");
    let blockchain_lock = blockchain.read().await;
    let tip_block: Option<Block> = blockchain_lock.get_tip_block()?;
    let height = blockchain_lock.block_height()?;
    drop(blockchain_lock);

    let response = if let Some(block) = tip_block {
        Message::LatestBlock(Some((block, height)))
    } else {
        Message::LatestBlock(None)
    };

    response.send_async(stream).await?;
    Ok(())
}

/// Handles a `GetBlockHeaders` request, sending back a list of headers from the specified range.
pub async fn handle_get_block_headers(
    stream: &mut TcpStream,
    from_index: u64,
    count: u32,
    blockchain: Arc<RwLock<Blockchain>>,
) -> Result<()> {
    debug!(
        "Handling GetBlockHeaders from index {} (count: {})",
        from_index, count
    );
    let mut headers = Vec::with_capacity(count as usize);
    let blockchain_lock = blockchain.read().await;

    for i in 0..count {
        if let Some(block) = blockchain_lock.get_block_by_index(from_index + i as u64)? {
            headers.push(block.header());
        } else {
            // Stop if we reach the end of the chain
            break;
        }
    }
    drop(blockchain_lock);

    Message::BlockHeaders(headers).send_async(stream).await?;
    Ok(())
}
