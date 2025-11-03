use anyhow::Context;
use chrono::Utc;
use log::{debug, error, info, warn};
use std::time::Duration;
use std::{io::ErrorKind, sync::Arc};
use tokio::net::{TcpStream, ToSocketAddrs};
use wisp_core::{
    blockchain::{AddBlockResult, Block},
    currency::Amount,
    network::Message,
    network::WalletStateSnapshot,
    sha256::Hash,
    transactions::{Transaction, TransactionOutput},
    utils::{calculate_block_reward, MerkleRoot},
};

use crate::{BLOCKCHAIN, NODES};

/// Handles all incoming P2P communication from a single connected peer.
/// This function runs in a loop, receiving and processing messages until the connection is closed or an error occurs.
pub async fn handle_connection<A: ToSocketAddrs + std::fmt::Display + Clone + Send + 'static>(
    socket: TcpStream,
    addr: A,
) -> Result<(), anyhow::Error> {
    // Timeout for idle connections.
    const IDLE_TIMEOUT_SECS: u64 = 120;
    let socket = Arc::new(tokio::sync::Mutex::new(socket));
    loop {
        let mut socket_guard = socket.lock().await;
        let message_result = match tokio::time::timeout(
            Duration::from_secs(IDLE_TIMEOUT_SECS),
            Message::receive_async(&mut *socket_guard),
        )
        .await
        {
            Ok(res) => res,
            Err(_) => {
                info!(
                    "Peer {} timed out due to inactivity. Closing connection.",
                    addr
                );
                return Ok(());
            }
        };
        let message = match message_result {
            Ok(message) => message,
            // Handle disconnection gracefully.
            Err(e) => {
                if e.kind() == ErrorKind::UnexpectedEof || e.kind() == ErrorKind::ConnectionReset {
                    info!("Peer {} disconnected.", addr);
                    return Ok(());
                } else {
                    warn!("Error receiving message from peer {}: {}", addr, e);
                    return Err(
                        anyhow::Error::new(e).context(format!("Error receiving from {}", addr))
                    );
                }
            }
        };

        use wisp_core::network::Message::*;
        match message {
            // These are messages a node should send, not receive. Receiving them is a protocol violation.
            NewTemplate(_)
            | Template(_)
            | TemplateValidity(_)
            | NodeList(_)
            | TransactionAcceptedConfirmation
            | BlockSubmittedConfirmation
            | WalletState(_)
            | MempoolInfo(_)
            | BlockInfo(_)
            | BlockRejected(_)
            | ChainSegment(_) => {
                warn!(
                    "Received unexpected message {:?} from {}, closing connection.",
                    message, addr
                );
                return Ok(());
            }
            // A peer is requesting a specific block by its index.
            FetchBlock(index_requested) => {
                let blockchain = BLOCKCHAIN.get().unwrap().read().await;
                debug!(
                    "Peer {} requested block by index: {}",
                    addr, index_requested
                );
                let block = match blockchain.get_block_by_index(index_requested) {
                    Ok(Some(b)) => b,
                    Ok(None) => {
                        warn!("Requested block index {} not found.", index_requested);
                        return Ok(());
                    }
                    Err(e) => {
                        error!("Error fetching block by index {}: {}", index_requested, e);
                        return Err(e.into());
                    }
                };

                let message = NewBlock(block);
                message
                    .send_async(&mut *socket_guard)
                    .await
                    .context("Failed to send NewBlock")?;
            }
            // A peer is requesting a block by index.
            FetchBlockInfo(index) => {
                let blockchain_read = BLOCKCHAIN.get().unwrap().read().await;
                debug!("Peer {} requested block info for index {}.", addr, index);
                let block_result = blockchain_read.get_block_by_index(index);
                let message = match block_result {
                    Ok(block_opt) => Message::BlockInfo(block_opt),
                    Err(_) => Message::BlockInfo(None),
                };

                message
                    .send_async(&mut *socket_guard)
                    .await
                    .context(format!("Failed to send BlockInfo for index {}", index))?;
            }
            // A peer is requesting a block by its hash.
            FetchBlockByHash(hash) => {
                debug!("Peer {} requested block info for hash {}.", addr, hash);
                let blockchain_read = BLOCKCHAIN.get().unwrap().read().await;
                let block_result = blockchain_read.get_block_by_hash(&hash);
                let message = match block_result {
                    Ok(block_opt) => Message::BlockInfo(block_opt),
                    Err(_) => Message::BlockInfo(None),
                };

                message
                    .send_async(&mut *socket_guard)
                    .await
                    .context(format!("Failed to send BlockInfo for hash {}", hash))?;
            }
            // A peer is asking for other nodes we know about.
            DiscoverNodes => {
                debug!("Peer {} requested node discovery.", addr);
                let nodes = NODES.iter().map(|x| x.key().clone()).collect::<Vec<_>>();
                let message = NodeList(nodes);
                message
                    .send_async(&mut *socket_guard)
                    .await
                    .context("Failed to send NodeList")?;
            }
            // A peer has sent us a new block. This is a critical part of the protocol.
            NewBlock(block) => {
                let received_block_hash = block.id().unwrap_or_default();
                info!(
                    "Received NewBlock message for index: {} (hash: {})",
                    block.index, received_block_hash
                );

                let mut blockchain = BLOCKCHAIN.get().unwrap().write().await;

                // Attempt to add the block to our blockchain.
                let add_result = blockchain.add_block(block.clone())?;

                match add_result {
                    AddBlockResult::Added => {
                        info!(
                            "✅ Block {} (index {}) accepted and added to chain.",
                            received_block_hash, block.index
                        );

                        // If the block was added, broadcast it to our other peers.
                        let block_to_broadcast = block.clone();
                        let originating_peer_addr = socket.lock().await.peer_addr()?.to_string();

                        tokio::spawn(async move {
                            let block_hash_for_log = block_to_broadcast.id().unwrap_or_default();
                            let peer_keys: Vec<String> =
                                NODES.iter().map(|p| p.key().clone()).collect();

                            for key in &peer_keys {
                                if *key == originating_peer_addr {
                                    continue;
                                }

                                if let Some(mut peer) = NODES.get_mut(key) {
                                    let message = Message::NewBlock(block_to_broadcast.clone());
                                    if message.send_async(peer.value_mut()).await.is_err() {
                                        warn!(
                                            "Failed to broadcast block {} to peer {}",
                                            block_hash_for_log, key
                                        );
                                    }
                                } else {
                                    warn!("Peer {} disappeared during block broadcast.", key);
                                }
                            }
                            info!(
                                "📢 Finished broadcasting block {} to peers.",
                                block_hash_for_log
                            );
                        });

                        // Drop the lock *before* waiting for the next message
                        drop(blockchain);
                    }
                    // A fork was detected. The node's main loop will handle reorg logic.
                    AddBlockResult::PotentialLongerForkDetected {
                        common_ancestor_index,
                        new_block_index: _,
                        new_block_hash: _,
                    } => {
                        warn!(
                            "Received block {} which is part of a potential longer fork. Common ancestor at index {}.",
                            received_block_hash, common_ancestor_index
                        );

                        // Spawn a task to fetch the new chain segment and attempt a reorg.
                        let socket_clone = Arc::clone(&socket);
                        tokio::spawn(async move {
                            // This inner function returns a Result, making error handling with `?` easy.
                            let reorg_logic = || async {
                                info!(
                                    "Requesting chain segment from peer starting at index {}.",
                                    common_ancestor_index + 1
                                );
                                let request_msg =
                                    Message::FetchChainSegment(common_ancestor_index + 1);
                                request_msg
                                    .send_async(&mut *socket_clone.lock().await)
                                    .await
                                    .context("Failed to request chain segment for reorg")?;

                                let received_message = tokio::time::timeout(
                                    Duration::from_secs(30),
                                    Message::receive_async(&mut *socket_clone.lock().await),
                                )
                                .await
                                .context("Timeout waiting for chain segment for reorg")??;

                                match received_message {
                                    Message::ChainSegment(new_chain_segment) => {
                                        info!(
                                            "Received chain segment of length {} for reorg.",
                                            new_chain_segment.len()
                                        );
                                        let mut blockchain =
                                            BLOCKCHAIN.get().unwrap().write().await;
                                        blockchain
                                            .reorganize_chain(
                                                new_chain_segment,
                                                common_ancestor_index,
                                            )
                                            .context("Chain reorganization failed")?;
                                        info!("Chain reorganization successful.");
                                    }
                                    other => {
                                        return Err(anyhow::anyhow!(
                                            "Received unexpected message during reorg attempt: {:?}",
                                            other
                                        ));
                                    }
                                }
                                Ok(())
                            };

                            // Execute the logic and log any error that occurs.
                            // The task itself will complete successfully regardless.
                            if let Err(e) = reorg_logic().await {
                                error!("Error during chain reorganization task: {:?}", e);
                            }
                        });
                    }
                    AddBlockResult::Rejected(reason) => {
                        warn!(
                            "❌ Block {} rejected due to invalidity: {}",
                            received_block_hash, reason
                        );
                        let rejection_message = Message::BlockRejected(reason);
                        if rejection_message
                            .send_async(&mut *socket_guard)
                            .await
                            .is_err()
                        {
                            warn!(
                                "Failed to send BlockRejected message back to peer {}.",
                                addr
                            );
                        }
                    }
                    AddBlockResult::ShorterForkRejected(reason) => {
                        warn!(
                            "❌ Block {} rejected (shorter fork): {}",
                            received_block_hash, reason
                        );
                        let rejection_message = Message::BlockRejected(reason);
                        if rejection_message
                            .send_async(&mut *socket_guard)
                            .await
                            .is_err()
                        {
                            warn!(
                                "Failed to send BlockRejected message back to peer {}.",
                                addr
                            );
                        }
                    }
                    AddBlockResult::OrphanedOrDisconnected(reason) => {
                        warn!(
                            "❌ Block {} rejected (orphaned): {}",
                            received_block_hash, reason
                        );
                        let rejection_message = Message::BlockRejected(reason);
                        if rejection_message
                            .send_async(&mut *socket_guard)
                            .await
                            .is_err()
                        {
                            warn!(
                                "Failed to send BlockRejected message back to peer {}.",
                                addr
                            );
                        }
                    }
                }
            }
            // A peer has sent us a new transaction.
            NewTransaction(transaction) => {
                let tx_hash = transaction.txid().unwrap_or_default();
                info!("Received NewTransaction message for hash: {}", tx_hash);
                let mut blockchain = BLOCKCHAIN.get().unwrap().write().await;

                match blockchain.add_to_mempool(transaction.clone()) {
                    Ok(_) => {
                        info!("Transaction {} added to mempool.", tx_hash);
                        let originating_peer_addr = socket.lock().await.peer_addr()?.to_string();

                        drop(blockchain);

                        // If the transaction is valid and added to our mempool, broadcast it to other peers.
                        let tx_to_broadcast = transaction;
                        tokio::spawn(async move {
                            let mut broadcast_count = 0;
                            for mut peer in NODES.iter_mut() {
                                if *peer.key() == originating_peer_addr {
                                    continue;
                                }
                                let message = Message::NewTransaction(tx_to_broadcast.clone());
                                if message.send_async(peer.value_mut()).await.is_ok() {
                                    broadcast_count += 1;
                                }
                            }
                            info!(
                                "📢 Broadcasted transaction {} to {} peers.",
                                tx_hash, broadcast_count
                            );
                        });
                    }
                    Err(e) => {
                        warn!("Transaction {} rejected: {}", tx_hash, e);

                        let rejection_message = Message::TransactionRejected(
                            tx_hash,
                            format!("Transaction Rejected: {}", e),
                        );
                        if rejection_message
                            .send_async(&mut *socket_guard)
                            .await
                            .is_err()
                        {
                            warn!(
                                "Failed to send TransactionRejected message back to peer {}.",
                                addr
                            );
                        }
                    }
                }
            }
            TransactionRejected(tx_hash, reason) => {
                warn!(
                    "Received TransactionRejected for tx {}: {}",
                    tx_hash, reason
                );
            }
            // A miner is asking us to validate a template they are working on to see if it's still valid.
            ValidateTemplate(block_template) => {
                let blockchain = BLOCKCHAIN.get().unwrap().read().await;
                let tip_hash = blockchain.get_tip_hash()?.unwrap_or_default();

                // A template is considered valid as long as its previous_hash matches our current chain tip.
                // This means no new block has been found since the template was issued.
                // The full validation of merkle root, fees, etc., will happen upon block submission.
                // This avoids race conditions where mempool order changes between template creation and validation.
                let status = block_template.previous_hash == tip_hash;

                let message = TemplateValidity(status);
                message
                    .send_async(&mut *socket_guard)
                    .await
                    .context(format!("Failed to send TemplateValidity to {}", addr))?;
            }
            // A miner is submitting a block they have successfully mined.
            SubmitTemplate(block) => {
                let submitted_block_hash = block.id().unwrap_or_default();
                info!(
                    "Received allegedly mined block with hash: {} at index {}",
                    submitted_block_hash, block.index
                );

                // Acquire a write lock, add the block, and then immediately drop the lock
                // to prevent deadlocks before awaiting on network I/O.
                let mut blockchain = BLOCKCHAIN.get().unwrap().write().await;
                let add_result = blockchain.add_block(block.clone())?;
                match add_result {
                    // The block was successfully added to the chain.
                    AddBlockResult::Added => {
                        // The block was successfully added to the chain.
                        let originating_peer_addr = socket_guard.peer_addr()?.to_string();
                        let block_for_broadcast = block.clone();

                        // Broadcast the newly mined block to the network.
                        tokio::spawn(async move {
                            let block_hash_for_log = block_for_broadcast.id().unwrap_or_default();
                            let peer_keys: Vec<String> =
                                NODES.iter().map(|p| p.key().clone()).collect();

                            for key in &peer_keys {
                                if *key == originating_peer_addr {
                                    debug!(
                                        "Skipping broadcast of mined block back to originator {}",
                                        key
                                    );
                                    continue;
                                }

                                if let Some(mut peer) = NODES.get_mut(key) {
                                    let message = Message::NewBlock(block_for_broadcast.clone());
                                    if message.send_async(peer.value_mut()).await.is_err() {
                                        warn!(
                                            "Failed to broadcast mined block {} to peer {}",
                                            block_hash_for_log, key
                                        );
                                    }
                                }
                            }
                            info!(
                                "📢 Broadcasted mined block {} to {} peers.",
                                block_hash_for_log,
                                peer_keys.len().saturating_sub(1)
                            );
                        });

                        // Confirm to the miner that their block was accepted.
                        let confirmation_message = Message::BlockSubmittedConfirmation;
                        confirmation_message.send_async(&mut *socket_guard).await.unwrap_or_else(|e| {
                            warn!("Failed to send BlockSubmittedConfirmation back to miner {}: {}", addr, e);
                        });
                        info!("Sent BlockSubmittedConfirmation to miner {}.", addr);

                        drop(blockchain);
                        continue;
                    }

                    AddBlockResult::Rejected(reason) => {
                        // The block was invalid.
                        warn!(
                            "❌ Mined block {} rejected due to invalidity: {}",
                            submitted_block_hash, reason
                        );

                        let rejection_message = Message::BlockRejected(reason);
                        if rejection_message
                            .send_async(&mut *socket_guard)
                            .await
                            .is_err()
                        {
                            warn!(
                                "Failed to send BlockRejected message back to miner {}.",
                                addr
                            );
                        } else {
                            info!("Sent BlockRejected message to miner.");
                        }
                    }
                    // This can happen if another block was found by someone else while the miner was submitting.
                    AddBlockResult::PotentialLongerForkDetected {
                        common_ancestor_index,
                        new_block_index,
                        new_block_hash: _,
                    } => {
                        warn!("Mined block {} (index {}) is part of a potential longer fork. Common ancestor at index {}. Signalling for full re-sync from self to other nodes.",
                            submitted_block_hash, new_block_index, common_ancestor_index
                        );

                        let confirmation_message = Message::BlockSubmittedConfirmation;
                        if confirmation_message
                            .send_async(&mut *socket_guard)
                            .await
                            .is_err()
                        {
                            warn!("Failed to send BlockSubmittedConfirmation back to miner {} after fork detection.", addr);
                        } else {
                            info!("Sent BlockSubmittedConfirmation to miner after fork detection.");
                        }

                        let originating_peer_addr = socket_guard.peer_addr()?.to_string();

                        tokio::spawn(async move {
                            let block_hash_for_log = block.id().unwrap_or_default();
                            let mut broadcast_count = 0;
                            for mut peer in NODES.iter_mut() {
                                if *peer.key() == originating_peer_addr {
                                    continue;
                                }
                                let message = Message::NewBlock(block.clone());
                                if message.send_async(peer.value_mut()).await.is_err() {
                                    warn!("Failed to broadcast newly mined (forking) block {} to peer {}", block_hash_for_log, peer.key());
                                } else {
                                    broadcast_count += 1;
                                }
                            }
                            info!(
                                "📢 Broadcasted newly mined (forking) block {} to {} peers.",
                                block_hash_for_log, broadcast_count
                            );
                        });
                        drop(blockchain);
                        continue;
                    }
                    AddBlockResult::ShorterForkRejected(reason) => {
                        warn!(
                            "❌ Mined block {} rejected (shorter fork): {}",
                            submitted_block_hash, reason
                        );
                        let rejection_message = Message::BlockRejected(reason);
                        if rejection_message
                            .send_async(&mut *socket_guard)
                            .await
                            .is_err()
                        {
                            warn!(
                                "Failed to send BlockRejected message back to miner {}.",
                                addr
                            );
                        } else {
                            info!("Sent BlockRejected message to miner.");
                        }
                    }
                    AddBlockResult::OrphanedOrDisconnected(reason) => {
                        warn!(
                            "❌ Mined block {} rejected (orphaned): {}",
                            submitted_block_hash, reason
                        );
                        let rejection_message = Message::BlockRejected(reason);
                        if rejection_message
                            .send_async(&mut *socket_guard)
                            .await
                            .is_err()
                        {
                            warn!(
                                "Failed to send BlockRejected message back to miner {}.",
                                addr
                            );
                        } else {
                            info!("Sent BlockRejected message to miner.");
                        }
                    }
                }
                info!("Finished processing SubmitTemplate.");
            }
            // A wallet is submitting a transaction to the network.
            SubmitTransaction(tx) => {
                let tx_hash = tx.txid().unwrap_or_default();
                info!("Received submitted transaction with hash: {}", tx_hash);
                let mut blockchain = BLOCKCHAIN.get().unwrap().write().await;
                let result = blockchain.add_to_mempool(tx.clone());

                match result {
                    Ok(()) => {
                        info!("Transaction {} accepted into mempool.", tx_hash);
                        let confirmation_message = Message::TransactionAcceptedConfirmation;
                        confirmation_message.send_async(&mut *socket_guard).await?;
                        info!("Sent TransactionAcceptedConfirmation to peer {}.", addr);

                        // Spawn a task to broadcast the new transaction to other peers.
                        // This is crucial for propagating the transaction through the network.
                        let addr_clone = addr.clone();
                        tokio::spawn(async move {
                            let originating_peer_addr = addr_clone.to_string();
                            for mut peer in NODES.iter_mut() {
                                if *peer.key() == originating_peer_addr {
                                    continue;
                                }
                                let message = Message::NewTransaction(tx.clone());
                                if message.send_async(peer.value_mut()).await.is_err() {
                                    warn!(
                                        "Failed to broadcast tx {} to peer {}",
                                        tx_hash,
                                        peer.key()
                                    );
                                }
                            }
                        });
                    }
                    Err(e) => {
                        warn!("Transaction {} rejected: {}", tx_hash, e);
                        let rejection_message = Message::TransactionRejected(
                            tx_hash,
                            format!("Transaction Rejected: {}", e),
                        );
                        if rejection_message
                            .send_async(&mut *socket_guard)
                            .await
                            .is_err()
                        {
                            warn!(
                                "Failed to send TransactionRejected message back to peer {}.",
                                addr
                            );
                        } else {
                            info!("Sent TransactionRejected message to peer.");
                        }
                    }
                }
                info!(
                    "Finished processing SubmitTransaction for hash {}.",
                    tx_hash
                );
            }
            // A miner is requesting a block template to start mining.
            FetchTemplate(pubkey) => {
                let blockchain = BLOCKCHAIN.get().unwrap().read().await;
                debug!(
                    "Peer {} requested a block template for pubkey {}. Mempool size: {}",
                    addr,
                    pubkey.fingerprint(),
                    blockchain.mempool().len()
                );

                // Gather transactions from the mempool, prioritized by fee.
                let mut transactions: Vec<Transaction> = blockchain
                    .get_mempool_transactions_for_block() // Assumes this method exists and sorts by fee
                    .into_iter()
                    .take(wisp_core::MAX_BLOCK_TRANSACTIONS - 1) // Convert to iterator before taking
                    .collect();

                let next_block_index = blockchain.block_height()? + 1;

                let block_reward = calculate_block_reward(next_block_index);
                // Calculate total fees from the included transactions.
                let mut total_fees = Amount::zero();
                for tx in &transactions {
                    if let Ok(fee) = blockchain.calculate_transaction_fee(tx) {
                        if let Ok(new_total) = total_fees + fee {
                            total_fees = new_total;
                        } else {
                            warn!("Fee summation overflowed while creating template. Some fees may be excluded.");
                            break;
                        }
                    }
                }

                // Create the coinbase transaction rewarding the miner.
                let mut coinbase_data = Vec::new();
                coinbase_data.extend_from_slice(&next_block_index.to_le_bytes());

                let coinbase_tx_output = TransactionOutput {
                    pubkey: pubkey.clone(),
                    value: (block_reward + total_fees)?,
                };
                let coinbase_input = wisp_core::transactions::TransactionInput {
                    outpoint: wisp_core::transactions::OutPoint {
                        txid: Hash::zero(),
                        vout: u32::MAX,
                    },
                    coinbase_data: Some(coinbase_data),
                    signature: None,
                };

                transactions.insert(
                    0,
                    Transaction::new(vec![coinbase_input], vec![coinbase_tx_output]),
                );

                let previous_hash = match blockchain.get_tip_hash()? {
                    Some(hash) => hash,
                    None => Hash::zero(),
                };

                // Construct the block template.
                // The miner is responsible for recalculating the final Merkle root after
                // inserting its extra_nonce, but we provide an initial valid one.
                let merkle_root = MerkleRoot::calculate(&transactions)
                    .context("Failed to calculate Merkle root for template")?;

                let next_target = blockchain
                    .calculate_next_target()
                    .context("Failed to calculate next target for template")?;

                let block = Block::new(
                    1,
                    Utc::now(),
                    0, // nonce, will be overwritten by miner
                    previous_hash,
                    merkle_root,
                    next_target,
                    next_block_index,
                    transactions,
                );

                let message = Template(block);
                message
                    .send_async(&mut *socket_guard)
                    .await
                    .context(format!("Failed to send Template to {}", addr))?;
            }
            // A wallet is requesting the latest block.
            FetchLatestBlock => {
                let blockchain_read = BLOCKCHAIN.get().unwrap().read().await;
                let height = blockchain_read.block_height()?;
                let latest_block = blockchain_read.get_block_by_index(height)?;

                let response = if let Some(block) = latest_block {
                    Message::LatestBlock(Some((block, height)))
                } else {
                    Message::LatestBlock(None)
                };
                response
                    .send_async(&mut *socket_guard)
                    .await
                    .context(format!("Failed to send LatestBlock to {}", addr))?;
            }
            LatestBlock(block_and_index) => {
                warn!(
                    "Received unexpected LatestBlock message from {}: {:?}",
                    addr, block_and_index
                );
            }
            // A peer is requesting the size of our mempool.
            FetchMempoolInfo => {
                let blockchain_read = BLOCKCHAIN.get().unwrap().read().await;
                let mempool_size = blockchain_read.mempool().len();
                let message = Message::MempoolInfo(mempool_size);
                message
                    .send_async(&mut *socket_guard)
                    .await
                    .context(format!("Failed to send MempoolInfo to {}", addr))?;
            }
            // A peer is checking if we are alive.
            Ping => {
                let pong_message = Message::Pong;
                pong_message // Corrected
                    .send_async(&mut *socket_guard)
                    .await
                    .context(format!("Failed to send Pong to {}", addr))?;
                debug!("Received Ping from {}, sent Pong.", addr);
            }
            Pong => {
                debug!("Received Pong from {}.", addr);
            }
            // A wallet is requesting the status of a specific transaction.
            FetchTransactionStatus(hash) => {
                let blockchain = BLOCKCHAIN.get().unwrap().read().await;
                debug!("Peer {} requested status for transaction {}", addr, hash);
                let status_message = blockchain.get_transaction_status(&hash);
                let message = Message::TransactionStatus {
                    hash,
                    status: status_message,
                };
                message
                    .send_async(&mut *socket_guard)
                    .await
                    .context(format!(
                        "Failed to send TransactionStatus for hash {}",
                        hash
                    ))?;
            }
            // A wallet is requesting an atomic snapshot of its state (UTXOs and pending transactions).
            FetchWalletState(pubkey) => {
                debug!(
                    "Peer {} requested atomic wallet state for pubkey {}",
                    addr,
                    pubkey.fingerprint()
                );
                let blockchain = BLOCKCHAIN.get().unwrap().read().await;

                let utxos = blockchain
                    .get_utxos_for_pubkey(&pubkey)
                    .into_iter()
                    .map(|(outpoint, output)| (outpoint, output))
                    .collect();

                let transactions = blockchain
                    .get_wallet_transaction_history(&pubkey)
                    .unwrap_or_default();

                let snapshot = WalletStateSnapshot {
                    transactions,
                    utxos,
                };

                let message = Message::WalletState(snapshot);
                message.send_async(&mut *socket_guard).await?;
                debug!("Sent full WalletState snapshot to peer {}", addr);
            }
            TransactionStatus { hash, status } => {
                warn!(
                    "Received unexpected TransactionStatus message from {} for hash {}: {:?}",
                    addr, hash, status
                );
            }
            // A peer is requesting a segment of our chain.
            FetchChainSegment(start_index) => {
                debug!(
                    "Peer {} requested chain segment from index {}",
                    addr, start_index
                );
                let blockchain = BLOCKCHAIN.get().unwrap().read().await;
                let mut segment = Vec::new();
                let current_height = blockchain.block_height()?;

                // Limit the number of blocks sent to avoid huge messages.
                const MAX_SEGMENT_LENGTH: u64 = 100;
                let end_index = (start_index + MAX_SEGMENT_LENGTH - 1).min(current_height);

                for i in start_index..=end_index {
                    if let Some(block) = blockchain.get_block_by_index(i)? {
                        segment.push(block);
                    } else {
                        break; // Stop if a block is missing
                    }
                }

                info!(
                    "Sending chain segment of length {} to peer {}",
                    segment.len(),
                    addr
                );
                let message = Message::ChainSegment(segment);
                message
                    .send_async(&mut *socket_guard)
                    .await
                    .context("Failed to send ChainSegment")?;
            }
        }
    }
}
