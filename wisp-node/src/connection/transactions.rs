use anyhow::Result;
use log::{debug, info, warn};
use std::{net::SocketAddr, sync::Arc};
use tokio::{net::TcpStream, sync::RwLock};
use wisp_core::{
    blockchain::Blockchain,
    network::{Message, WalletMessage, WalletStateSnapshot},
    signatures::PublicKey,
    transactions::Transaction,
};

/// Handles a `SubmitTransaction` message from a peer.
pub async fn handle_submit_transaction(
    stream: &mut TcpStream,
    tx: Transaction,
    sender_addr: SocketAddr,
    blockchain: Arc<RwLock<Blockchain>>,
) -> Result<()> {
    let tx_hash = tx.txid()?;
    info!("Received transaction {} for submission.", tx_hash);

    let mut blockchain_lock = blockchain.write().await;

    match blockchain_lock.add_to_mempool(tx.clone()) {
        Ok(_) => {
            info!("Transaction {} accepted into mempool.", tx_hash);

            drop(blockchain_lock);

            Message::Wallet(WalletMessage::TransactionAcceptedConfirmation)
                .send_async(stream)
                .await?;

            tokio::spawn(async move {
                broadcast_transaction(tx, sender_addr).await;
            });
        }
        Err(e) => {
            warn!("Transaction {} rejected: {}", tx_hash, e);
            Message::Wallet(WalletMessage::TransactionRejected(tx_hash, e.to_string()))
                .send_async(stream)
                .await?;
        }
    }

    Ok(())
}

/// Handles a `FetchWalletState` request from a peer (typically a wallet client).
pub async fn handle_fetch_wallet_state(
    stream: &mut TcpStream,
    pubkey: PublicKey,
    blockchain: Arc<RwLock<Blockchain>>,
) -> Result<()> {
    debug!(
        "Handling FetchWalletState for pubkey: {}",
        pubkey.fingerprint()
    );

    let blockchain_lock = blockchain.read().await;
    let utxos = blockchain_lock.get_utxos_for_pubkey(&pubkey);
    let transactions = blockchain_lock.get_wallet_transaction_history(&pubkey)?;

    let snapshot = WalletStateSnapshot {
        transactions,
        utxos,
    };

    Message::Wallet(WalletMessage::WalletState(snapshot))
        .send_async(stream)
        .await?;
    Ok(())
}

/// Broadcasts a new, valid transaction to all connected peers except the one it came from.
async fn broadcast_transaction(tx: Transaction, original_sender: SocketAddr) {
    let message = Message::Wallet(WalletMessage::NewTransaction(tx));
    let mut peers_to_remove = Vec::new();

    for mut peer in crate::NODES.iter_mut() {
        let addr = peer.key().clone();
        if addr == original_sender.to_string() {
            continue;
        }

        let mut stream_lock = peer.value_mut().lock().await;
        if let Err(e) = message.send_async(&mut *stream_lock).await {
            warn!(
                "Failed to broadcast transaction to {}: {}. Marking for removal.",
                addr, e
            );
            peers_to_remove.push(addr);
        }
    }

    for addr in peers_to_remove {
        crate::NODES.remove(&addr);
    }
}
