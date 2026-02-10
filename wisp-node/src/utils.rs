use std::time::Duration;

use anyhow::{anyhow, Result};
use log::{debug, error, info, warn};
use tokio::net::TcpStream;
use tokio::time;
use wisp_core::blockchain::AddBlockResult;
use wisp_core::network::{ChainMessage, Message, P2PMessage};

pub async fn populate_connections(nodes: &[String], self_port: u16) -> Result<()> {
    info!("Attempting to connect to nodes: {:?}", nodes);
    for node in nodes {
        info!("Connecting to node: {}", node);
        match time::timeout(Duration::from_secs(5), TcpStream::connect(&node)).await {
            Ok(Ok(stream)) => {
                if let Err(e) = perform_handshake(stream, node, self_port).await {
                    warn!("Handshake with {} failed: {}", node, e);
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
    Ok(())
}

async fn perform_handshake(mut stream: TcpStream, node_addr: &str, self_port: u16) -> Result<()> {
    info!("Performing handshake with {}", node_addr);

    let self_addr = if let Some(public_addr) = crate::PUBLIC_ADDR.get() {
        public_addr.clone()
    } else {
        let local_ip = stream.local_addr()?.ip();
        format!("{}:{}", local_ip, self_port)
    };

    Message::P2P(P2PMessage::Hello(self_addr))
        .send_async(&mut stream)
        .await?;

    Message::Chain(ChainMessage::FetchLatestBlock)
        .send_async(&mut stream)
        .await?;

    // Wait for their response and send our height
    if let Ok(Ok(Message::Chain(ChainMessage::LatestBlock(Some((_, height)))))) =
        time::timeout(Duration::from_secs(5), Message::receive_async(&mut stream)).await
    {
        let blockchain = crate::BLOCKCHAIN.get().unwrap().read().await;
        let our_height = blockchain.block_height()?;

        if height > our_height {
            warn!(
                "Peer {} has a longer chain ({} vs our {}). Sync will be attempted after discovery.",
                node_addr, height, our_height
            );
        }

        // Send our height so they can decide if they need to sync from us.
        if let Some(tip) = blockchain.get_tip_block()? {
            Message::Chain(ChainMessage::LatestBlock(Some((tip, our_height))))
                .send_async(&mut stream)
                .await?;
        } else {
            Message::Chain(ChainMessage::LatestBlock(None))
                .send_async(&mut stream)
                .await?;
        }
    } else {
        return Err(anyhow!(
            "Failed to exchange chain heights with {}",
            node_addr
        ));
    };

    Message::P2P(P2PMessage::DiscoverNodes)
        .send_async(&mut stream)
        .await?;
    info!("Sent DiscoverNodes to {}", node_addr);

    match time::timeout(Duration::from_secs(5), Message::receive_async(&mut stream)).await {
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

    use std::sync::Arc;
    use tokio::sync::Mutex as AsyncMutex;
    if !crate::NODES.contains_key(node_addr) {
        let stream_arc = Arc::new(AsyncMutex::new(stream));
        crate::NODES.insert(node_addr.to_string(), stream_arc);
        info!("Handshake successful. Added initial node: {}", node_addr);

        let stream_clone = crate::NODES.get(node_addr).unwrap().clone();
        let addr_clone = node_addr.to_string();
        let socket_addr = stream_clone.lock().await.peer_addr()?;
        tokio::spawn(async move {
            let _ =
                crate::connection::handle_connection(stream_clone, socket_addr, Some(addr_clone))
                    .await;
        });
    }

    Ok(())
}

pub async fn find_longest_chain_node(
    blockchain: &wisp_core::blockchain::Blockchain,
) -> Result<(String, u64)> {
    info!("Finding node with the longest blockchain...");
    let mut longest_name = String::new();
    let mut longest_count = blockchain.block_height()?.saturating_add(1);
    info!(
        "Local chain length is {}. Searching for peers with longer chains...",
        longest_count
    );

    let peers_to_query: Vec<String> = crate::NODES.iter().map(|p| p.key().clone()).collect();

    for node_addr in peers_to_query {
        if let Some(mut peer) = crate::NODES.get_mut(&node_addr) {
            let mut stream_lock = peer.value_mut().lock().await;

            debug!("Querying {} for blockchain length", node_addr);
            let message = Message::Chain(ChainMessage::FetchLatestBlock);

            if let Err(e) = message.send_async(&mut *stream_lock).await {
                warn!(
                    "Failed to send FetchLatestBlock to {}: {}. Skipping.",
                    node_addr, e
                );
                continue;
            }

            debug!("Sent FetchLatestBlock to {}", node_addr);

            match time::timeout(
                Duration::from_secs(5),
                Message::receive_async(&mut *stream_lock),
            )
            .await
            {
                Ok(Ok(Message::Chain(ChainMessage::LatestBlock(Some((_, remote_height)))))) => {
                    let remote_block_count = remote_height + 1;
                    debug!(
                        "Received LatestBlock with height {} from {}",
                        remote_height, node_addr
                    );
                    if remote_block_count > longest_count {
                        info!(
                            "New longest blockchain: {} blocks from {}",
                            remote_block_count, node_addr
                        );
                        longest_count = remote_block_count;
                        longest_name = node_addr.clone();
                    }
                }
                Ok(Ok(Message::Chain(ChainMessage::LatestBlock(None)))) => {
                    debug!("Peer {} reported an empty chain.", node_addr);
                }
                Ok(Ok(message)) => {
                    warn!("Unexpected message from {}: {:?}", node_addr, message);
                }
                Ok(Err(e)) => {
                    warn!("Error receiving latest block from {}: {:?}", node_addr, e);
                }
                Err(_) => {
                    warn!("Timeout waiting for LatestBlock from {}", node_addr);
                }
            }
        }
    }

    info!(
        "Longest chain found on node: {} with length: {}",
        longest_name, longest_count
    );
    Ok((longest_name, longest_count))
}

pub async fn download_blockchain_from_new_connection(
    node_addr: &str,
    target_block_count: u64,
) -> Result<()> {
    info!(
        "Establishing new connection to {} for blockchain download.",
        node_addr
    );

    let mut stream =
        match time::timeout(Duration::from_secs(10), TcpStream::connect(node_addr)).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                return Err(anyhow!(
                    "Failed to establish new connection for blockchain download: {}",
                    e
                ));
            }
            Err(_) => {
                return Err(anyhow!(
                    "Timeout establishing new connection for blockchain download."
                ));
            }
        };

    download_blockchain_with_existing_stream(&mut stream, node_addr, target_block_count).await
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

    for i in (local_chain_height + 1)..=(target_block_count - 1) {
        debug!("Attempting to fetch block index {} from {}", i, node_addr);

        let message = Message::Chain(ChainMessage::FetchBlock(i as u64));

        if let Err(e) = message.send_async(stream).await {
            error!("Failed to send FetchBlock({}) to {}: {}", i, node_addr, e);
            return Err(anyhow!(
                "Failed to send FetchBlock({}) to {}: {}",
                i,
                node_addr,
                e
            ));
        }

        match time::timeout(Duration::from_secs(5), Message::receive_async(stream)).await {
            Ok(Ok(Message::Chain(ChainMessage::NewBlock(block)))) => {
                debug!(
                    "Received NewBlock for index {} from {}",
                    block.index, node_addr
                );

                let mut blockchain = crate::BLOCKCHAIN.get().unwrap().write().await;

                if block.index != i as u64 {
                    error!(
                        "Received block with unexpected index. Expected {}, got {}. Block Hash: {}",
                        i,
                        block.index,
                        block.id().unwrap_or_default()
                    );
                    return Err(anyhow!(
                        "Received block with unexpected index. Expected {}, got {}.",
                        i,
                        block.index
                    ));
                }

                let add_result = blockchain.add_block(block);
                match add_result? {
                    AddBlockResult::Added => debug!(
                        "Block with index {} successfully added during download. Current chain height: {}",
                        i,
                        blockchain.block_height()?
                    ),
                    other_result => {
                        error!(
                            "Failed to add block {} from {}: {:?}. Aborting blockchain download.",
                            i, node_addr, other_result
                        );
                        return Err(anyhow!(
                            "Unexpected result while adding block {} from {}: {:?}",
                            i,
                            node_addr,
                            other_result
                        ));
                    }
                }
            }
            Ok(Ok(message)) => {
                error!(
                    "Unexpected message {:?} from {} while downloading block {}",
                    message, node_addr, i
                );
                return Err(anyhow!(
                    "Unexpected message {:?} from {} while downloading block {}",
                    message,
                    node_addr,
                    i,
                ));
            }
            Ok(Err(e)) => {
                error!("Error receiving block {} from {}: {}", i, node_addr, e);
                return Err(anyhow!(
                    "Error receiving block {} from {}: {}",
                    i,
                    node_addr,
                    e
                ));
            }
            Err(_) => {
                error!("Timeout downloading block {} from {}", node_addr, i);
                return Err(anyhow!(
                    "Timeout downloading block {} from {}",
                    node_addr,
                    i
                ));
            }
        }
    }
    info!(
        "Blockchain download from {} completed successfully.",
        node_addr
    );
    Ok(())
}

pub async fn cleanup() {
    let mut interval = time::interval(time::Duration::from_secs(30));
    info!("Cleanup task started");
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
