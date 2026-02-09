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
        let message = match Message::receive_async(&mut *stream_lock).await {
            Ok(msg) => msg,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    info!("Connection closed by peer {}", addr);
                    return Ok(());
                }
                return Err(anyhow!("Failed to receive message from {}: {}", addr, e));
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
            Message::Chain(ChainMessage::LatestBlock(Some((_, their_height)))) => {
                let our_height = { blockchain.read().await.block_height()? };
                if their_height > our_height {
                    warn!(
                        "Peer {} has a longer chain ({} vs our {}). Spawning background task to sync.",
                        addr, their_height, our_height
                    );

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
