use anyhow::{anyhow, Result};
use log::{error, info, warn};
use std::net::SocketAddr;
use tokio::net::TcpStream;
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
            Message::FetchTemplate(pubkey) => {
                mining::handle_fetch_template(&mut stream, pubkey, blockchain.clone()).await
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
