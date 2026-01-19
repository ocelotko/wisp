use anyhow::{anyhow, Result};
use log::{debug, error, info, warn};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::{net::TcpStream, sync::Mutex as AsyncMutex};
use wisp_core::network::{ChainMessage, Message, MiningMessage, P2PMessage, WalletMessage};

pub mod blocks;
pub mod mining;
pub mod peers;
pub mod sync;
pub mod transactions;

/// A guard that removes a peer from the global NODES list when dropped.
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
/// It listens for incoming `Message` enums and dispatches them to the appropriate handler function. It now takes an Arc<AsyncMutex<TcpStream>> to allow for shared access.
pub async fn handle_connection(
    stream_arc: Arc<AsyncMutex<TcpStream>>,
    addr: SocketAddr,
    known_peer_addr: Option<String>,
) -> Result<()> {
    // Initialize the guard. If we initiated the connection, we know the address.
    let mut peer_guard = PeerGuard {
        addr: known_peer_addr,
    };

    // Get a clonable handle to the global blockchain state.
    let blockchain = crate::BLOCKCHAIN.get().unwrap().clone();

    // The main connection handling loop.
    loop {
        // Lock the stream to receive a message. The lock is released at the end of the expression.
        let mut stream_lock = stream_arc.lock().await;

        // Wait for a message from the peer.
        let message = match Message::receive_async(&mut *stream_lock).await {
            Ok(msg) => msg,
            Err(e) => {
                // If the error indicates a closed connection, we can exit gracefully.
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    info!("Connection closed by peer {}", addr);
                    return Ok(());
                }
                // For other errors, we log it and terminate the connection.
                return Err(anyhow!("Failed to receive message from {}: {}", addr, e));
            }
        };

        // Drop the lock here so that message handlers can acquire it if they need to send a response.
        drop(stream_lock);

        // Dispatch the message to the appropriate handler based on its type.
        let result = match message {
            // The first message from a connecting peer should be Hello.
            Message::P2P(P2PMessage::Hello(public_addr)) => {
                info!(
                    "Peer {} announced its public address: {}",
                    addr, public_addr
                );
                // If we didn't know the address (incoming connection), add it now.
                if peer_guard.addr.is_none() {
                    crate::NODES.insert(public_addr.clone(), stream_arc.clone());
                    peer_guard.addr = Some(public_addr);
                } else {
                    // If we already knew it, just update the guard to be sure.
                    debug!("Received Hello from known peer: {}", public_addr);
                }
                Ok(())
            }
            // Block and Chain Sync Messages
            Message::Chain(ChainMessage::NewBlock(block)) => {
                blocks::handle_new_block(block, blockchain.clone()).await
            }
            Message::Chain(ChainMessage::FetchBlock(index)) => {
                let mut stream_lock = stream_arc.lock().await;
                sync::handle_fetch_block(&mut *stream_lock, index, blockchain.clone()).await
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
            // This is part of the handshake. A peer sends us their height, and we check if we need to sync.
            Message::Chain(ChainMessage::LatestBlock(Some((_, their_height)))) => {
                // We only act on this if we are the *receiving* end of the connection.
                // The node that initiates the connection handles its own sync logic at startup.
                let our_height = { blockchain.read().await.block_height()? };
                if their_height > our_height {
                    // The peer has a longer chain. We should try to sync from them.
                    // We spawn this as a background task to avoid blocking the connection handler.
                    // The download function will establish its own connection for the sync process.
                    warn!(
                        "Peer {} has a longer chain ({} vs our {}). Spawning background task to sync.",
                        addr, their_height, our_height
                    );
                    // Use the public address the peer gave us in the `Hello` message.
                    // Fallback to the address we see, though it's likely an ephemeral port.
                    if let Some(addr_to_sync_from) = peer_guard.addr.clone() {
                        tokio::spawn(async move {
                            if let Err(e) = crate::utils::download_blockchain_from_new_connection(
                                &addr_to_sync_from,
                                their_height + 1,
                            )
                            .await
                            {
                                warn!("Background sync from {} failed: {}", addr_to_sync_from, e);
                            }
                        });
                    } else {
                        warn!("Cannot sync from peer {} because we don't know its public address (no Hello message received).", addr);
                    }
                } else {
                    // This is just informational logging.
                    info!(
                        "Peer {} has chain height {}. Our height is {}.",
                        addr, their_height, our_height
                    );
                }
                Ok(())
            }
            Message::Chain(ChainMessage::LatestBlock(None)) => {
                Ok(()) // Peer has an empty chain, nothing to do.
            }
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

            // Transaction and Mempool Messages
            Message::Wallet(WalletMessage::SubmitTransaction(tx)) => {
                let mut stream_lock = stream_arc.lock().await;
                transactions::handle_submit_transaction(
                    &mut *stream_lock,
                    tx,
                    addr,
                    blockchain.clone(),
                )
                .await
            }
            Message::Wallet(WalletMessage::FetchWalletState(pubkey)) => {
                let mut stream_lock = stream_arc.lock().await;
                transactions::handle_fetch_wallet_state(
                    &mut *stream_lock,
                    pubkey,
                    blockchain.clone(),
                )
                .await
            }

            // Mining Messages
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

            // Peer Discovery Messages
            Message::P2P(P2PMessage::DiscoverNodes) => {
                let mut stream_lock = stream_arc.lock().await;
                peers::handle_discover_nodes(&mut *stream_lock).await
            }
            Message::P2P(P2PMessage::Ping) => {
                let mut stream_lock = stream_arc.lock().await;
                peers::handle_ping(&mut *stream_lock).await
            }

            // Any other message type is considered unexpected in this context.
            other => {
                warn!(
                    "Received unhandled or unexpected message type from {}: {:?}",
                    addr, other
                );
                Ok(())
            }
        };

        // If any handler returns an error, we log it and close the connection.
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
