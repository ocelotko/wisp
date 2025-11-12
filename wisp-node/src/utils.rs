use std::time::Duration;

use anyhow::{anyhow, Result};
use log::{debug, error, info, warn};
use tokio::net::TcpStream;
use tokio::time;
use wisp_core::blockchain::AddBlockResult;
use wisp_core::network::Message;

/// Connects to a list of initial seed nodes.
pub async fn populate_connections(nodes: &[String], self_port: u16) -> Result<()> {
    info!("Attempting to connect to nodes: {:?}", nodes);
    for node in nodes {
        info!("Connecting to node: {}", node);
        match time::timeout(Duration::from_secs(5), TcpStream::connect(&node)).await {
            Ok(Ok(stream)) => {
                // The stream is now owned by this scope. We will pass it to the handshake
                // and then move it into the NODES map if successful.
                if let Err(e) = perform_handshake(stream, node, self_port).await {
                    warn!("Handshake with {} failed: {}", node, e);
                }
                // Note: The stream is consumed by perform_handshake. If the handshake is
                // successful, the stream is now living in the NODES map. If it fails,
                // the stream is dropped, and the connection is closed, which is the
                // desired behavior.
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

/// Performs a structured handshake with a newly connected peer.
/// This function is called by the node that *initiates* the connection.
async fn perform_handshake(mut stream: TcpStream, node_addr: &str, self_port: u16) -> Result<()> {
    info!("Performing handshake with {}", node_addr);

    // --- Step 1: Announce ourselves ---
    // We assume our IP is the one the peer sees. We tell them our listening port.
    let self_addr = format!("{}:{}", stream.local_addr()?.ip(), self_port);
    Message::Hello(self_addr).send_async(&mut stream).await?;

    // --- Step 2: Two-Way Height Exchange ---
    Message::FetchLatestBlock.send_async(&mut stream).await?;

    // Wait for their response and send our height
    if let Ok(Ok(Message::LatestBlock(Some((_, height))))) =
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
            Message::LatestBlock(Some((tip, our_height)))
                .send_async(&mut stream)
                .await?;
        } else {
            Message::LatestBlock(None).send_async(&mut stream).await?;
        }
    } else {
        return Err(anyhow!(
            "Failed to exchange chain heights with {}",
            node_addr
        ));
    };

    // --- Step 3: Node Discovery ---
    Message::DiscoverNodes.send_async(&mut stream).await?;
    info!("Sent DiscoverNodes to {}", node_addr);

    match time::timeout(Duration::from_secs(5), Message::receive_async(&mut stream)).await {
        Ok(Ok(Message::NodeList(child_nodes))) => {
            info!("Received NodeList from {}: {:?}", node_addr, child_nodes);
            // In a real-world scenario, you might want to connect to these child nodes here.
            // For now, we just log them.
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

    // --- Step 4: Finalize Connection ---
    // If the handshake was successful, add the peer to our global map.
    // We move the stream into the map, giving it a permanent home.
    use std::sync::Arc;
    use tokio::sync::Mutex as AsyncMutex;
    if !crate::NODES.contains_key(node_addr) {
        let stream_arc = Arc::new(AsyncMutex::new(stream));
        crate::NODES.insert(node_addr.to_string(), stream_arc);
        info!("Handshake successful. Added initial node: {}", node_addr);
    }

    Ok(())
}

/// Queries all known peers to find out which one has the longest blockchain.
pub async fn find_longest_chain_node(
    blockchain: &wisp_core::blockchain::Blockchain,
) -> Result<(String, u64)> {
    info!("Finding node with the longest blockchain...");
    let mut longest_name = String::new();
    // Initialize with our own chain length. If no peer has a longer one, we won't sync.
    let mut longest_count = blockchain.block_height()?.saturating_add(1);
    info!(
        "Local chain length is {}. Searching for peers with longer chains...",
        longest_count
    );

    // Create a list of peers to query to avoid holding the DashMap lock.
    let peers_to_query: Vec<String> = crate::NODES.iter().map(|p| p.key().clone()).collect();

    for node_addr in peers_to_query {
        // Re-acquire a mutable reference to the stream for this peer.
        if let Some(mut peer) = crate::NODES.get_mut(&node_addr) {
            let mut stream_lock = peer.value_mut().lock().await;

            debug!("Querying {} for blockchain length", node_addr);
            let message = Message::FetchLatestBlock;

            // Send a message asking for the peer's chain height.
            if let Err(e) = message.send_async(&mut *stream_lock).await {
                warn!(
                    "Failed to send FetchLatestBlock to {}: {}. Skipping.",
                    node_addr, e
                );
                continue;
            }

            debug!("Sent FetchLatestBlock to {}", node_addr);

            // Wait for the peer's response.
            match time::timeout(
                Duration::from_secs(5),
                Message::receive_async(&mut *stream_lock),
            )
            .await
            {
                Ok(Ok(Message::LatestBlock(Some((_, remote_height))))) => {
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
                Ok(Ok(Message::LatestBlock(None))) => {
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

/// Downloads the blockchain from a specified peer up to a target block count.
/// This is used for initial chain synchronization.
pub async fn download_blockchain_from_new_connection(
    node_addr: &str,
    target_block_count: u64,
) -> Result<()> {
    info!(
        "Establishing new connection to {} for blockchain download.",
        node_addr
    );

    // Establish a dedicated connection for the download process.
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

    // Loop from our current height to the target height, fetching one block at a time.
    for i in (local_chain_height + 1)..=(target_block_count - 1) {
        debug!("Attempting to fetch block index {} from {}", i, node_addr);

        let message = Message::FetchBlock(i as u64);

        // Request the block.
        if let Err(e) = message.send_async(stream).await {
            error!("Failed to send FetchBlock({}) to {}: {}", i, node_addr, e);
            return Err(anyhow!(
                "Failed to send FetchBlock({}) to {}: {}",
                i,
                node_addr,
                e
            ));
        }

        // Wait for the block to be sent back.
        match time::timeout(Duration::from_secs(5), Message::receive_async(stream)).await {
            Ok(Ok(Message::NewBlock(block))) => {
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

                // Add the received block to our local blockchain.
                let add_result = blockchain.add_block(block);
                match add_result? {
                    AddBlockResult::Added => debug!(
                        "Block with index {} successfully added during download. Current chain height: {}",
                        i,
                        blockchain.block_height()?
                    ),
                    other_result => {
                        // During initial sync, we expect a clean series of `Added` results.
                        // Any other result (Rejected, Orphaned, ForkDetected) indicates a desync or a malicious peer.
                        // It's safest to abort the download.
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

/// A background task that periodically cleans up the mempool by removing old transactions.
pub async fn cleanup() {
    let mut interval = time::interval(time::Duration::from_secs(30));
    info!("Cleanup task started");
    loop {
        interval.tick().await;
        debug!("Cleaning the mempool from old transactions");
        let mut blockchain = crate::BLOCKCHAIN.get().unwrap().write().await;
        blockchain.clear_mempool();
    }
}
