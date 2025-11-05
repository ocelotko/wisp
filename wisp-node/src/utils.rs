use std::time::Duration;

use anyhow::{anyhow, Result};
use log::{debug, error, info, warn};
use tokio::net::TcpStream;
use tokio::time;
use wisp_core::blockchain::AddBlockResult;
use wisp_core::network::Message;

/// Connects to a list of initial seed nodes and discovers their peers.
/// This function bootstraps the node's connection to the network.
pub async fn populate_connections(nodes: &[String]) -> Result<()> {
    info!("Attempting to connect to nodes: {:?}", nodes);
    for node in nodes {
        info!("Connecting to node: {}", node);
        // Connect to the seed node with a timeout.
        match time::timeout(Duration::from_secs(5), TcpStream::connect(&node)).await {
            Ok(Ok(mut stream)) => {
                info!("Successfully connected to {}", node);
                let message = Message::DiscoverNodes;
                if let Err(e) = message.send_async(&mut stream).await {
                    warn!("Failed to send DiscoverNodes to {}: {}", node, e);
                    continue;
                }
                info!("Sent DiscoverNodes to {}", node);

                // Wait for the seed node to respond with its list of peers.
                match time::timeout(Duration::from_secs(5), Message::receive_async(&mut stream))
                    .await
                {
                    Ok(Ok(Message::NodeList(child_nodes))) => {
                        info!("Received NodeList from {}: {:?}", node, child_nodes);
                        for child_node in child_nodes {
                            // Attempt to connect to each newly discovered peer.
                            debug!("Adding node {}", child_node);
                            match time::timeout(
                                Duration::from_secs(5),
                                TcpStream::connect(&child_node),
                            )
                            .await
                            {
                                Ok(Ok(new_stream)) => {
                                    if !crate::NODES.contains_key(&child_node) {
                                        let node_name_for_map = child_node.clone();
                                        crate::NODES.insert(node_name_for_map, new_stream);
                                        debug!("Added new child node: {}", child_node);
                                    } else {
                                        debug!(
                                            "Child node {} already known, skipping connection.",
                                            child_node
                                        );
                                    }
                                }
                                Ok(Err(e)) => {
                                    warn!("Failed to connect to child node {}: {}", child_node, e);
                                }
                                Err(_) => {
                                    warn!("Timeout connecting to child node: {}", child_node);
                                }
                            }
                        }
                    }
                    Ok(Ok(message)) => {
                        warn!("Unexpected message from {}: {:?}", node, message);
                    }
                    Ok(Err(e)) => {
                        warn!("Error receiving message from {}: {}", node, e);
                    }
                    Err(_) => {
                        warn!("Timeout receiving message from {}", node);
                    }
                }
                if !crate::NODES.contains_key(node) {
                    crate::NODES.insert(node.clone(), stream);
                    info!("Added initial node: {}", node);
                } else {
                    debug!(
                        "Initial node {} already known, skipping re-insertion.",
                        node
                    );
                }
            }
            Ok(Err(e)) => {
                warn!("Failed to connect to {}: {}", node, e);
            }
            Err(_) => {
                warn!("Timeout connecting to node: {}", node);
            }
        }
    }
    Ok(())
}

/// Queries all known peers to find out which one has the longest blockchain.
pub async fn find_longest_chain_node() -> Result<(String, u64)> {
    info!("Finding node with the longest blockchain...");
    let mut longest_name = String::new();
    let mut longest_count = 0;

    // Iterate through all connected peers in the `NODES` map.
    for mut peer in crate::NODES.iter_mut() {
        let node_addr = peer.key().clone();
        let stream = peer.value_mut();

        debug!("Querying {} for blockchain length", node_addr);

        let message = Message::FetchLatestBlock;

        // Send a message asking for the peer's chain height.
        if let Err(e) = message.send_async(stream).await {
            warn!(
                "Failed to send FetchLatestBlock to {}: {}. Skipping.",
                node_addr, e
            );
            continue;
        }

        debug!("Sent FetchLatestBlock to {}", node_addr);

        // Wait for the peer's response.
        match time::timeout(Duration::from_secs(5), Message::receive_async(stream)).await {
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

    info!(
        "Longest chain found on node: {} with length: {}",
        longest_name, longest_count
    );
    Ok((longest_name, longest_count))
}

/// Downloads the blockchain from a specified peer up to a target block count.
/// This is used for initial chain synchronization.
pub async fn download_blockchain(node: &str, target_block_count: u64) -> Result<()> {
    let initial_height = crate::BLOCKCHAIN
        .get()
        .unwrap()
        .read()
        .await
        .block_height()?;
    info!(
        "Starting blockchain download from peer: {} (target: {} blocks, local: {} blocks)",
        node,
        target_block_count,
        initial_height + 1
    );

    // Establish a dedicated connection for the download process.
    let mut stream = match time::timeout(Duration::from_secs(10), TcpStream::connect(node)).await {
        Ok(Ok(s)) => {
            info!(
                "Successfully established new connection to {} for download.",
                node
            );
            s
        }
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
        debug!("Attempting to fetch block index {} from {}", i, node);

        let message = Message::FetchBlock(i as u64);

        // Request the block.
        if let Err(e) = message.send_async(&mut stream).await {
            error!("Failed to send FetchBlock({}) to {}: {}", i, node, e);
            return Err(anyhow!(
                "Failed to send FetchBlock({}) to {}: {}",
                i,
                node,
                e
            ));
        }

        // Wait for the block to be sent back.
        match time::timeout(Duration::from_secs(5), Message::receive_async(&mut stream)).await {
            Ok(Ok(Message::NewBlock(block))) => {
                debug!("Received NewBlock for index {} from {}", block.index, node);

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
                            i, node, other_result
                        );
                        return Err(anyhow!(
                            "Unexpected result while adding block {} from {}: {:?}",
                            i,
                            node,
                            other_result
                        ));
                    }
                }
            }
            Ok(Ok(message)) => {
                error!(
                    "Unexpected message {:?} from {} while downloading block {}",
                    message, node, i
                );
                return Err(anyhow!(
                    "Unexpected message {:?} from {} while downloading block {}",
                    message,
                    node,
                    i,
                ));
            }
            Ok(Err(e)) => {
                error!("Error receiving block {} from {}: {}", i, node, e);
                return Err(anyhow!("Error receiving block {} from {}: {}", i, node, e));
            }
            Err(_) => {
                error!("Timeout downloading block {} from {}", node, i);
                return Err(anyhow!("Timeout downloading block {} from {}", node, i));
            }
        }
    }
    info!("Blockchain download from {} completed successfully.", node);
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
