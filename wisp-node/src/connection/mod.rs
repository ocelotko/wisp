use anyhow::{anyhow, Result};
use log::{error, info, warn};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::{net::TcpStream, sync::Mutex as AsyncMutex};
use wisp_core::network::Message;

pub mod blocks;
pub mod mining;
pub mod peers;
pub mod sync;
pub mod transactions;
/// The main loop for handling messages from a single connected peer.
/// It listens for incoming `Message` enums and dispatches them to the appropriate handler function. It now takes an Arc<AsyncMutex<TcpStream>> to allow for shared access.
pub async fn handle_connection(
    stream_arc: Arc<AsyncMutex<TcpStream>>,
    addr: SocketAddr,
) -> Result<()> {
    // The address of the peer as they see themselves. This is crucial for NAT traversal.
    let mut peer_public_addr: Option<String> = None;

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
            Message::Hello(public_addr) => {
                info!(
                    "Peer {} announced its public address: {}",
                    addr, public_addr
                );
                // Add the peer to our global list so we can broadcast to them.
                // We use the public address they provided as the key.
                // Note: This assumes the `handle_connection` takes ownership of the stream,
                // which it does. We need to handle the case where the stream is already
                // in the NODES map if we initiated the connection.
                // For now, we'll just insert, but a more robust solution might check first.
                // The `DashMap` will just update the value if the key exists.
                crate::NODES.insert(public_addr.clone(), stream_arc.clone());
                peer_public_addr = Some(public_addr);
                Ok(())
            }
            // Block and Chain Sync Messages
            Message::NewBlock(block) => blocks::handle_new_block(block, blockchain.clone()).await,
            Message::FetchBlock(index) => {
                let mut stream_lock = stream_arc.lock().await;
                sync::handle_fetch_block(&mut *stream_lock, index, blockchain.clone()).await
            }
            Message::FetchBlockByHash(hash) => {
                let mut stream_lock = stream_arc.lock().await;
                sync::handle_fetch_block_by_hash(&mut *stream_lock, hash, blockchain.clone()).await
            }
            Message::FetchLatestBlock => {
                let mut stream_lock = stream_arc.lock().await;
                sync::handle_fetch_latest_block(&mut *stream_lock, blockchain.clone()).await
            }
            // This is part of the handshake. A peer sends us their height, and we check if we need to sync.
            Message::LatestBlock(Some((_, their_height))) => {
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
                    if let Some(addr_to_sync_from) = peer_public_addr.clone() {
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
            Message::LatestBlock(None) => {
                Ok(()) // Peer has an empty chain, nothing to do.
            }
            Message::GetBlockHeaders { from_index, count } => {
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
            Message::SubmitTransaction(tx) => {
                let mut stream_lock = stream_arc.lock().await;
                transactions::handle_submit_transaction(
                    &mut *stream_lock,
                    tx,
                    addr,
                    blockchain.clone(),
                )
                .await
            }
            Message::FetchWalletState(pubkey) => {
                let mut stream_lock = stream_arc.lock().await;
                transactions::handle_fetch_wallet_state(
                    &mut *stream_lock,
                    pubkey,
                    blockchain.clone(),
                )
                .await
            }

            // Mining Messages
            Message::FetchTemplate(pubkey, coinbase_message) => {
                let mut stream_lock = stream_arc.lock().await;
                mining::handle_fetch_template(
                    &mut *stream_lock,
                    pubkey,
                    coinbase_message,
                    blockchain.clone(),
                )
                .await
            }
            Message::SubmitTemplate(pubkey, block, coinbase_message) => {
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
            Message::DiscoverNodes => {
                let mut stream_lock = stream_arc.lock().await;
                peers::handle_discover_nodes(&mut *stream_lock).await
            }
            Message::Ping => {
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
