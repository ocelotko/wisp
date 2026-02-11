use std::time::Duration;

use anyhow::{anyhow, Result};
use log::{debug, info, warn};
use tokio::net::TcpStream;
use tokio::time;
use wisp_core::blockchain::AddBlockResult;
use wisp_core::{
    blockchain::Blockchain,
    network::{ChainMessage, Message, P2PMessage},
};

pub async fn populate_connections(
    nodes: &[String],
    self_port: u16,
    blockchain: &Blockchain,
) -> Result<Option<(String, u64)>> {
    info!("Attempting to connect to nodes: {:?}", nodes);
    let mut longest_peer: Option<(String, u64)> = None;

    for node in nodes {
        info!("Connecting to node: {}", node);
        match time::timeout(Duration::from_secs(5), TcpStream::connect(&node)).await {
            Ok(Ok(mut stream)) => {
                if let Err(e) =
                    perform_handshake(&mut stream, node, self_port, blockchain, &mut longest_peer)
                        .await
                {
                    warn!("Handshake with {} failed: {}", node, e);
                } else if !crate::NODES.contains_key(node) {
                    // Handshake was successful, move the stream into the global map
                    // and spawn a handler for it.
                    let stream_arc = std::sync::Arc::new(tokio::sync::Mutex::new(stream));
                    crate::NODES.insert(node.to_string(), stream_arc.clone());
                    info!("Handshake successful. Added initial node: {}", node);

                    let addr_clone = node.to_string();
                    let socket_addr = stream_arc.lock().await.peer_addr()?;
                    tokio::spawn(async move {
                        let _ = crate::connection::handle_connection(
                            stream_arc,
                            socket_addr,
                            Some(addr_clone),
                        )
                        .await;
                    });
                }
            }
            Ok(Err(e)) => {
                warn!("Failed to connect to {}: {}", node, e);
            }
            Err(_) => {
                warn!("Timeout connecting to initial node: {}", node);
            }
        }
    }
    Ok(longest_peer)
}

async fn perform_handshake(
    stream: &mut TcpStream,
    node_addr: &str,
    self_port: u16,
    blockchain: &Blockchain,
    longest_peer: &mut Option<(String, u64)>,
) -> Result<()> {
    info!("Performing handshake with {}", node_addr);

    let self_addr = if let Some(public_addr) = crate::PUBLIC_ADDR.get() {
        public_addr.clone()
    } else {
        let local_ip = stream.local_addr()?.ip();
        format!("{}:{}", local_ip, self_port)
    };

    Message::P2P(P2PMessage::Hello(self_addr))
        .send_async(stream)
        .await?;

    Message::Chain(ChainMessage::FetchLatestBlock)
        .send_async(stream)
        .await?;

    // Wait for their response and send our height
    if let Ok(Ok(Message::Chain(ChainMessage::LatestBlock(Some((_, height)))))) =
        time::timeout(Duration::from_secs(5), Message::receive_async(stream)).await
    {
        let our_height = blockchain.block_height()?;

        if height > our_height {
            warn!(
                "Peer {} has a longer chain ({} vs our {}). Sync will be attempted after discovery.",
                node_addr, height, our_height
            );
        }

        // Track the peer with the longest chain discovered so far.
        let current_longest_height = longest_peer.as_ref().map(|(_, h)| *h).unwrap_or(our_height);
        if height > current_longest_height {
            *longest_peer = Some((node_addr.to_string(), height));
        }

        // Send our height so they can decide if they need to sync from us.
        if let Some(tip) = blockchain.get_tip_block()? {
            Message::Chain(ChainMessage::LatestBlock(Some((tip, our_height))))
                .send_async(stream)
                .await?;
        } else {
            Message::Chain(ChainMessage::LatestBlock(None))
                .send_async(stream)
                .await?;
        }
    } else {
        return Err(anyhow!(
            "Failed to exchange chain heights with {}",
            node_addr
        ));
    };

    Message::P2P(P2PMessage::DiscoverNodes)
        .send_async(stream)
        .await?;
    info!("Sent DiscoverNodes to {}", node_addr);

    match time::timeout(Duration::from_secs(5), Message::receive_async(stream)).await {
        Ok(Ok(Message::P2P(P2PMessage::NodeList(child_nodes)))) => {
            info!("Received NodeList from {}: {:?}", node_addr, child_nodes);

            for child in child_nodes {
                if child != node_addr && !crate::NODES.contains_key(&child) {
                    debug!("Discovered new peer: {}", child);
                }
            }
        }
        Ok(Ok(other)) => {
            return Err(anyhow!(
                "Unexpected message {:?} from {} during node discovery.",
                other,
                node_addr
            ));
        }
        Ok(Err(e)) => return Err(anyhow!("Error receiving NodeList: {}", e)),
        Err(_) => return Err(anyhow!("Timeout receiving NodeList from {}", node_addr)),
    }

    Ok(())
}

pub async fn download_blockchain_with_existing_stream(
    stream: &mut TcpStream,
    node_addr: &str,
    target_block_count: u64,
) -> Result<()> {
    let local_chain_height = crate::BLOCKCHAIN
        .get()
        .unwrap()
        .read()
        .await
        .block_height()?;

    if (local_chain_height + 1) >= target_block_count {
        info!(
            "Local chain height ({}) is already >= target count ({}). No blocks to download.",
            local_chain_height, target_block_count
        );
        return Ok(());
    }

    debug!(
        "Starting block download loop from index {} to {}.",
        local_chain_height + 1,
        target_block_count - 1
    );

    const BATCH_SIZE: u64 = 100;
    const BATCH_TIMEOUT_SECS: u64 = 30;
    let mut next_block_to_process = local_chain_height + 1;

    while next_block_to_process < target_block_count {
        let batch_end = (next_block_to_process + BATCH_SIZE).min(target_block_count);
        let num_to_request = batch_end - next_block_to_process;

        info!(
            "Requesting block batch from {} to {}...",
            next_block_to_process,
            batch_end - 1
        );

        // --- Send a batch of requests (pipelining) ---
        for i in next_block_to_process..batch_end {
            let message = Message::Chain(ChainMessage::FetchBlockInfo(i));
            if let Err(e) = message.send_async(stream).await {
                return Err(anyhow!(
                    "Failed to send FetchBlockInfo({}) to {}: {}",
                    i,
                    node_addr,
                    e
                ));
            }
        }

        // --- Receive a batch of responses ---
        let mut received_blocks = Vec::with_capacity(num_to_request as usize);
        for _ in 0..num_to_request {
            match time::timeout(
                Duration::from_secs(BATCH_TIMEOUT_SECS),
                Message::receive_async(stream),
            )
            .await
            {
                Ok(Ok(Message::Chain(ChainMessage::BlockInfo(Some(block))))) => {
                    received_blocks.push(block);
                }
                Ok(Ok(Message::Chain(ChainMessage::BlockInfo(None)))) => {
                    // The peer doesn't have a block we requested. This is a fatal error for the sync process with this peer.
                    let failed_index = next_block_to_process + received_blocks.len() as u64;
                    return Err(anyhow!(
                        "Sync failed: Peer {} does not have block at index {}. Try syncing from another peer.",
                        node_addr,
                        failed_index
                    ));
                }
                Ok(Ok(other_msg)) => {
                    return Err(anyhow!(
                        "Unexpected message {:?} from {} while downloading block batch",
                        other_msg,
                        node_addr
                    ));
                }
                Ok(Err(e)) => {
                    return Err(anyhow!(
                        "Network error receiving block batch from {}: {}",
                        node_addr,
                        e
                    ));
                }
                Err(_) => {
                    return Err(anyhow!("Timeout receiving block batch from {}", node_addr));
                }
            }
        }

        // Blocks can arrive out of order, so sort them before processing.
        received_blocks.sort_by_key(|b| b.index);

        // --- Process the batch of blocks ---
        let mut blockchain = crate::BLOCKCHAIN.get().unwrap().write().await;
        for (i, block) in received_blocks.into_iter().enumerate() {
            let expected_index = next_block_to_process + i as u64;
            if block.index != expected_index {
                return Err(anyhow!(
                    "Received out-of-sequence block. Expected index {}, got {}.",
                    expected_index,
                    block.index
                ));
            }

            match blockchain.add_block(block)? {
                AddBlockResult::Added => {
                    debug!(
                        "Block {} added during batch sync. Chain height: {}",
                        expected_index,
                        blockchain.block_height()?
                    );
                }
                other_result => {
                    return Err(anyhow!(
                        "Failed to add block {} from {}: {:?}. Aborting sync.",
                        expected_index,
                        node_addr,
                        other_result
                    ));
                }
            }
        }

        next_block_to_process = batch_end;
    }

    info!(
        "Blockchain download from {} completed successfully.",
        node_addr
    );
    Ok(())
}

pub async fn cleanup(interval_secs: u64) {
    if interval_secs == 0 {
        info!("Mempool cleanup task is disabled.");
        return;
    }
    let mut interval = time::interval(time::Duration::from_secs(interval_secs));
    info!(
        "Mempool cleanup task started with {}s interval",
        interval_secs
    );
    loop {
        interval.tick().await;
        debug!("Cleaning the mempool from old transactions");
        let mut blockchain = crate::BLOCKCHAIN.get().unwrap().write().await;
        blockchain.clear_mempool();
        if let Err(e) = blockchain.save_mempool_snapshot() {
            warn!("Failed to save mempool snapshot during cleanup: {}", e);
        }
    }
}
