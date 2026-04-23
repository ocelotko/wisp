use anyhow::{anyhow, Result};
use log::{debug, error, info, warn};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::{
    net::TcpStream,
    sync::Mutex as AsyncMutex,
    time::{timeout, Duration},
};
use wisp_core::network::{ChainMessage, Message, MiningMessage, P2PMessage, WalletMessage};

pub mod blocks;
pub mod mining;
pub mod peers;
pub mod sync;
pub mod transactions;

/// A filter to determine which peers a broadcast should be sent to.
pub enum BroadcastFilter {
    /// Send to all peers.
    All,
    /// Send to all peers except for the one specified by its stream Arc.
    AllExcept(Arc<AsyncMutex<TcpStream>>),
}

/// Broadcasts a message to filtered peers.
/// Removes peers that cause a send error.
pub async fn broadcast(message: &Message, filter: BroadcastFilter, log_verb: &str) {
    let mut peers_to_remove = Vec::new();

    // Collect Arcs first to avoid holding DashMap shard locks during I/O
    let peers: Vec<(String, Arc<AsyncMutex<TcpStream>>)> = crate::NODES
        .iter()
        .filter(|peer| match &filter {
            BroadcastFilter::All => true,
            BroadcastFilter::AllExcept(arc) => !Arc::ptr_eq(peer.value(), arc),
        })
        .map(|p| (p.key().clone(), p.value().clone()))
        .collect();

    for (addr, stream_arc) in peers {
        let mut stream_lock = stream_arc.lock().await;
        if let Err(e) = message.send_async(&mut *stream_lock).await {
            warn!(
                "Failed to broadcast {} to {}: {}. Marking for removal.",
                log_verb, addr, e
            );
            peers_to_remove.push(addr);
        }
    }

    // Clean up connections that failed during the broadcast.
    for addr in peers_to_remove {
        crate::NODES.remove(&addr);
    }
}

const PEER_TIMEOUT: Duration = Duration::from_secs(120);
const PING_TIMEOUT: Duration = Duration::from_secs(10);

struct PeerGuard {
    addr: Option<String>,
}

impl Drop for PeerGuard {
    fn drop(&mut self) {
        if let Some(addr) = &self.addr {
            info!("Peer {} disconnected. Removing from active peers.", addr);
            crate::NODES.remove(addr);
        }
    }
}

/// The main loop for handling messages from a single connected peer.
pub async fn handle_connection(
    stream_arc: Arc<AsyncMutex<TcpStream>>,
    addr: SocketAddr,
    known_peer_addr: Option<String>,
) -> Result<()> {
    let mut peer_guard = PeerGuard {
        addr: known_peer_addr,
    };

    let blockchain = crate::BLOCKCHAIN.get().unwrap().clone();

    loop {
        let mut stream_lock = stream_arc.lock().await;
        let message_future = Message::receive_async(&mut *stream_lock);

        let message = match timeout(PEER_TIMEOUT, message_future).await {
            Ok(Ok(msg)) => msg, // Message received within timeout
            Ok(Err(e)) => {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    info!("Connection closed by peer {}", addr);
                    return Ok(());
                }
                return Err(anyhow!("Failed to receive message from {}: {}", addr, e));
            }
            Err(_) => {
                // Timeout elapsed
                info!(
                    "Peer {} has been idle. Sending a ping to check liveness.",
                    addr
                );
                if let Err(e) = Message::P2P(P2PMessage::Ping)
                    .send_async(&mut *stream_lock)
                    .await
                {
                    warn!(
                        "Failed to send ping to {}: {}. Closing connection.",
                        addr, e
                    );
                    return Ok(());
                }

                // We expect any message back quickly, preferably a Pong.
                let response_future = Message::receive_async(&mut *stream_lock);
                match timeout(PING_TIMEOUT, response_future).await {
                    Ok(Ok(Message::P2P(P2PMessage::Pong))) => {
                        debug!("Received pong from {}. Connection is alive.", addr);
                        continue; // Go back to waiting for a message
                    }
                    Ok(Ok(other_message)) => {
                        // Any other message also proves liveness. Process it.
                        other_message
                    }
                    _ => {
                        // Timeout or error receiving response
                        warn!(
                            "Did not receive a timely response from {}. Closing connection.",
                            addr
                        );
                        return Ok(());
                    }
                }
            }
        };

        drop(stream_lock);

        let result = match message {
            Message::P2P(P2PMessage::Hello(public_addr)) => {
                info!(
                    "Peer {} announced its public address: {}",
                    addr, public_addr
                );
                if peer_guard.addr.is_none() {
                    crate::NODES.insert(public_addr.clone(), stream_arc.clone());
                    peer_guard.addr = Some(public_addr);
                } else {
                    debug!("Received Hello from known peer: {}", public_addr);
                }
                Ok(())
            }
            Message::Chain(ChainMessage::NewBlock(block)) => {
                blocks::handle_new_block(block, blockchain.clone(), stream_arc.clone()).await
            }
            Message::Chain(ChainMessage::FetchBlockByHash(hash)) => {
                let mut stream_lock = stream_arc.lock().await;
                sync::handle_fetch_block_by_hash(&mut *stream_lock, hash, blockchain.clone()).await
            }
            Message::Chain(ChainMessage::FetchBlockInfo(index)) => {
                let mut stream_lock = stream_arc.lock().await;
                sync::handle_fetch_block_info(&mut *stream_lock, index, blockchain.clone()).await
            }
            Message::Chain(ChainMessage::FetchLatestBlock) => {
                let mut stream_lock = stream_arc.lock().await;
                sync::handle_fetch_latest_block(&mut *stream_lock, blockchain.clone()).await
            }
            Message::Chain(ChainMessage::LatestBlock(Some((_, their_height)))) => {
                let our_height = { blockchain.read().await.block_height()? };
                if their_height > our_height {
                    warn!(
                        "Peer {} has a longer chain ({} vs our {}). Spawning background task to sync.",
                        addr, their_height, our_height
                    );

                    let stream_for_sync = stream_arc.clone();
                    let peer_addr_for_log =
                        peer_guard.addr.clone().unwrap_or_else(|| addr.to_string());
                    tokio::spawn(async move {
                        info!("Starting background sync with peer {}", peer_addr_for_log);
                        let mut stream_lock = stream_for_sync.lock().await;
                        if let Err(e) = crate::utils::download_blockchain_with_existing_stream(
                            &mut *stream_lock,
                            &peer_addr_for_log,
                            their_height + 1,
                        )
                        .await
                        {
                            warn!(
                                "Background sync from {} failed: {}. The connection may be closed.",
                                peer_addr_for_log, e
                            );
                        }
                    });
                } else {
                    info!(
                        "Peer {} has chain height {}. Our height is {}.",
                        addr, their_height, our_height
                    );
                }
                Ok(())
            }
            Message::Chain(ChainMessage::LatestBlock(None)) => Ok(()),
            Message::Chain(ChainMessage::GetBlockHeaders { from_index, count }) => {
                let mut stream_lock = stream_arc.lock().await;
                sync::handle_get_block_headers(
                    &mut *stream_lock,
                    from_index,
                    count,
                    blockchain.clone(),
                )
                .await
            }

            Message::Wallet(WalletMessage::SubmitTransaction(tx)) => {
                transactions::handle_submit_transaction(stream_arc.clone(), tx, blockchain.clone())
                    .await
            }
            Message::Wallet(WalletMessage::FetchWalletState(pubkey, script_hashes)) => {
                let mut stream_lock = stream_arc.lock().await;
                transactions::handle_fetch_wallet_state(
                    &mut *stream_lock,
                    pubkey,
                    script_hashes,
                    blockchain.clone(),
                )
                .await
            }

            Message::Mining(MiningMessage::FetchTemplate(pubkey, coinbase_message)) => {
                let mut stream_lock = stream_arc.lock().await;
                mining::handle_fetch_template(
                    &mut *stream_lock,
                    pubkey,
                    coinbase_message,
                    blockchain.clone(),
                )
                .await
            }
            Message::Mining(MiningMessage::SubmitTemplate(pubkey, block, coinbase_message)) => {
                let mut stream_lock = stream_arc.lock().await;
                mining::handle_submit_template(
                    &mut *stream_lock,
                    pubkey,
                    block,
                    coinbase_message,
                    blockchain.clone(),
                )
                .await
            }

            Message::P2P(P2PMessage::DiscoverNodes) => {
                let mut stream_lock = stream_arc.lock().await;
                peers::handle_discover_nodes(&mut *stream_lock).await
            }
            Message::P2P(P2PMessage::Ping) => {
                let mut stream_lock = stream_arc.lock().await;
                peers::handle_ping(&mut *stream_lock).await
            }

            other => {
                warn!(
                    "Received unhandled or unexpected message type from {}: {:?}",
                    addr, other
                );
                Ok(())
            }
        };

        if let Err(e) = result {
            error!(
                "Error processing message from {}: {}. Closing connection.",
                addr, e
            );
            break;
        }
    }

    Ok(())
}
