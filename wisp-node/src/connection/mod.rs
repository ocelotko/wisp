use anyhow::{anyhow, Result};
use log::{error, info, warn};
use std::net::SocketAddr;
use tokio::{net::TcpStream, time};
use wisp_core::network::Message;

pub mod blocks;
pub mod mining;
pub mod peers;
pub mod sync;
pub mod transactions;
/// The main loop for handling messages from a single connected peer.
/// It listens for incoming `Message` enums and dispatches them to the appropriate handler function.
pub async fn handle_connection(mut stream: TcpStream, addr: SocketAddr) -> Result<()> {
    // Get a clonable handle to the global blockchain state.
    let blockchain = crate::BLOCKCHAIN.get().unwrap().clone();

    // --- Bidirectional Sync Handshake ---
    // 1. Send our latest block info to the new peer so they know our height.
    let (our_tip, our_height) = {
        let bc = blockchain.read().await;
        (bc.get_tip_block()?, bc.block_height()?)
    };
    if let Some(tip) = our_tip {
        Message::LatestBlock(Some((tip, our_height)))
            .send_async(&mut stream)
            .await?;
    } else {
        // We have an empty chain
        Message::LatestBlock(None).send_async(&mut stream).await?;
    }

    // 2. Ask the new peer for their latest block info.
    Message::FetchLatestBlock.send_async(&mut stream).await?;

    // 3. Wait for their response and decide if we need to sync from them.
    // This is the same logic as the startup sync, but happens for every new connection.
    if let Ok(Ok(Message::LatestBlock(Some((_, their_height))))) = time::timeout(
        time::Duration::from_secs(5),
        Message::receive_async(&mut stream),
    )
    .await
    {
        if their_height > our_height {
            warn!(
                "Peer {} has a longer chain ({} vs our {}). Attempting to sync.",
                addr, their_height, our_height
            );
            // The download_blockchain function handles the sync process.
            crate::utils::download_blockchain(&addr.to_string(), their_height + 1).await?;
        }
    }

    loop {
        // Wait for a message from the peer.
        let message = match Message::receive_async(&mut stream).await {
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

        // Dispatch the message to the appropriate handler based on its type.
        let result = match message {
            // Block and Chain Sync Messages
            Message::NewBlock(block) => blocks::handle_new_block(block, blockchain.clone()).await,
            Message::FetchBlock(index) => {
                sync::handle_fetch_block(&mut stream, index, blockchain.clone()).await
            }
            Message::FetchBlockByHash(hash) => {
                sync::handle_fetch_block_by_hash(&mut stream, hash, blockchain.clone()).await
            }
            Message::FetchLatestBlock => {
                sync::handle_fetch_latest_block(&mut stream, blockchain.clone()).await
            }
            // This is the other side of the handshake. A peer receives our height.
            Message::LatestBlock(Some((_, their_height))) => {
                let our_height = blockchain.read().await.block_height()?;
                if their_height > our_height {
                    warn!(
                        "Peer {} has a longer chain ({} vs our {}). Attempting to sync.",
                        addr, their_height, our_height
                    );
                    // The download_blockchain function handles the sync process.
                    crate::utils::download_blockchain(&addr.to_string(), their_height + 1).await?;
                }
                Ok(())
            }
            Message::LatestBlock(None) => {
                Ok(()) // Peer has an empty chain, nothing to do.
            }
            Message::GetBlockHeaders { from_index, count } => {
                sync::handle_get_block_headers(&mut stream, from_index, count, blockchain.clone())
                    .await
            }

            // Transaction and Mempool Messages
            Message::SubmitTransaction(tx) => {
                transactions::handle_submit_transaction(&mut stream, tx, addr, blockchain.clone())
                    .await
            }
            Message::FetchWalletState(pubkey) => {
                transactions::handle_fetch_wallet_state(&mut stream, pubkey, blockchain.clone())
                    .await
            }

            // Mining Messages
            Message::FetchTemplate(pubkey, coinbase_message) => {
                mining::handle_fetch_template(
                    &mut stream,
                    pubkey,
                    coinbase_message,
                    blockchain.clone(),
                )
                .await
            }
            Message::SubmitTemplate(pubkey, block) => {
                mining::handle_submit_template(&mut stream, pubkey, block, blockchain.clone()).await
            }

            // Peer Discovery Messages
            Message::DiscoverNodes => peers::handle_discover_nodes(&mut stream).await,
            Message::Ping => peers::handle_ping(&mut stream).await,

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
