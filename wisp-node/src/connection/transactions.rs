use anyhow::Result;
use log::{debug, info, warn};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;
use tokio::{net::TcpStream, sync::RwLock};
use wisp_core::{
    blockchain::Blockchain,
    network::{Message, WalletMessage, WalletStateSnapshot},
    signatures::PublicKey,
    transactions::Transaction,
};

/// Handles a `SubmitTransaction` message from a peer.
pub async fn handle_submit_transaction(
    sender_stream_arc: Arc<AsyncMutex<TcpStream>>,
    tx: Transaction,
    blockchain: Arc<RwLock<Blockchain>>,
) -> Result<()> {
    let tx_hash = tx.txid()?;
    info!("Received transaction {} for submission.", tx_hash);

    let mut blockchain_lock = blockchain.write().await;
    let mut sender_stream_lock = sender_stream_arc.lock().await;

    match blockchain_lock.add_to_mempool(tx.clone()) {
        Ok(_) => {
            info!("Transaction {} accepted into mempool.", tx_hash);

            drop(blockchain_lock);

            Message::Wallet(WalletMessage::TransactionAcceptedConfirmation)
                .send_async(&mut *sender_stream_lock)
                .await?;

            let sender_for_broadcast = Arc::clone(&sender_stream_arc);
            tokio::spawn(async move {
                broadcast_transaction(tx, sender_for_broadcast).await;
            });
        }
        Err(e) => {
            warn!("Transaction {} rejected: {}", tx_hash, e);
            Message::Wallet(WalletMessage::TransactionRejected(tx_hash, e.to_string()))
                .send_async(&mut *sender_stream_lock)
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
async fn broadcast_transaction(tx: Transaction, original_sender_arc: Arc<AsyncMutex<TcpStream>>) {
    let tx_hash = tx.txid().unwrap_or_default();
    info!("Broadcasting transaction {} to other peers.", tx_hash);
    let message = Message::Wallet(WalletMessage::NewTransaction(tx));
    let filter = crate::connection::BroadcastFilter::AllExcept(original_sender_arc);
    crate::connection::broadcast(&message, filter, "transaction").await;
}
