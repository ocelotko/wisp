use crate::blockchain::Blockchain;
use anyhow::Result;
use bincode::config::standard as bincode_config;
use chrono::{DateTime, Utc};
use log::{debug, error};
use std::collections::HashSet;

use crate::{
    network::{TransactionStatus, WalletTransactionInfo},
    sha256::Hash,
    signatures::PublicKey,
    transactions::{OutPoint, Transaction, TransactionOutput},
};

impl Blockchain {
    /// Gets all UTXOs belonging to a specific public key from the in-memory UTXO set.
    pub fn get_utxos_for_pubkey(&self, pubkey: &PublicKey) -> Vec<(OutPoint, TransactionOutput)> {
        self.utxo_set
            .utxos
            .iter()
            .filter_map(|(outpoint, (_marked, output))| {
                (output.pubkey == *pubkey).then_some((*outpoint, output.clone()))
            })
            .collect()
    }

    /// Determines the status of a transaction by checking the mempool and the database.
    /// Returns `Pending`, `Confirmed`, or `NotFound`.
    pub fn get_transaction_status(&self, tx_hash: &Hash) -> TransactionStatus {
        if self.mempool.contains_key(tx_hash) {
            debug!("Transaction {} found in mempool.", tx_hash);
            return TransactionStatus::Pending;
        }

        let key = format!("tx_location_{}", tx_hash);
        // Check the database for a `tx_location` entry, which maps a tx hash to its block index.
        if let Ok(Some(ivec)) = self.db.get(&key) {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&ivec);
            let block_index = u64::from_be_bytes(bytes);

            if let Ok(Some(block)) = self.get_block_by_index(block_index) {
                if let Ok(block_hash) = block.id() {
                    return TransactionStatus::Confirmed {
                        block_hash,
                        block_index,
                    };
                }
            }
        } else if let Err(e) = self.db.get(&key) {
            error!("Database error checking tx_location: {}", e);
        }

        debug!(
            "Transaction {} not found in mempool or confirmed blocks.",
            tx_hash
        );
        TransactionStatus::NotFound
    }

    /// Retrieves a transaction and its associated metadata (block height, timestamp).
    /// It first checks the mempool, then falls back to the database.
    pub fn get_transaction_with_details(
        &self,
        tx_hash: &Hash,
    ) -> Result<Option<(Transaction, Option<u64>, DateTime<Utc>)>> {
        // Check the mempool first for unconfirmed transactions.
        if let Some((timestamp, tx, _)) = self.mempool.get(tx_hash) {
            return Ok(Some((tx.clone(), None, *timestamp)));
        }

        // If not in mempool, check the database for a confirmed transaction.
        let key = format!("tx_location_{}", tx_hash);
        if let Some(ivec) = self.db.get(&key)? {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&ivec);
            let block_index = u64::from_be_bytes(bytes);

            if let Some(block) = self.get_block_by_index(block_index)? {
                if let Some(tx) = block
                    .transactions
                    .iter()
                    .find(|t| t.txid().ok() == Some(*tx_hash))
                {
                    return Ok(Some((tx.clone(), Some(block.index), block.timestamp)));
                }
            }
        }

        Ok(None)
    }

    /// Returns the total number of confirmed transactions in the blockchain.
    pub fn get_total_transaction_count(&self) -> Result<u64> {
        self.get_total_transaction_count_from_db()
    }

    /// Finds a specific transaction output by its `OutPoint`.
    /// It checks the live UTXO set first, then falls back to searching the entire chain history.
    pub fn find_output_by_outpoint_in_chain_or_utxos(
        &self,
        outpoint: &OutPoint,
    ) -> Result<Option<TransactionOutput>> {
        if let Some((_, output)) = self.utxo_set.utxos.get(outpoint) {
            return Ok(Some(output.clone()));
        }

        let tx_hash = outpoint.txid;

        if let Some((tx, _, _)) = self.get_transaction_with_details(&tx_hash)? {
            if let Some(output) = tx.outputs.get(outpoint.vout as usize) {
                return Ok(Some(output.clone()));
            }
        }

        Ok(None)
    }

    /// Finds a specific transaction output by its `OutPoint` by searching the database only.
    /// This is useful for operations that need to look at historical state, like reorgs.
    pub fn find_output_by_outpoint_in_db(
        &self,
        outpoint: &OutPoint,
    ) -> Result<Option<TransactionOutput>> {
        Self::find_output_by_outpoint_in_db_static(&self.db, outpoint)
    }

    /// A static version of `find_output_by_outpoint_in_db` that takes a `Db` reference directly.
    /// This is used to break borrow checker conflicts, such as during a reorg.
    pub fn find_output_by_outpoint_in_db_static(
        db: &sled::Db,
        outpoint: &OutPoint,
    ) -> Result<Option<TransactionOutput>> {
        let tx_hash = outpoint.txid;

        // This logic is a simplified, DB-only version of `get_transaction_with_details`.
        // It avoids using `&self` and the mempool.
        let key = format!("tx_location_{}", tx_hash);
        if let Some(ivec) = db.get(&key)? {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&ivec);
            let block_index = u64::from_be_bytes(bytes);

            // Nested logic to get block from index
            let hash_key = format!("index_{}", block_index);
            if let Some(hash_ivec) = db.get(hash_key)? {
                let (hash, _): (Hash, _) =
                    bincode::decode_from_slice(&hash_ivec, bincode_config())?;

                // Nested logic to get block from hash
                let block_key = format!("block_{}", hash);
                if let Some(block_ivec) = db.get(block_key)? {
                    let (block, _): (crate::blockchain::Block, _) =
                        bincode::decode_from_slice(&block_ivec, bincode_config())?;
                    if let Some(tx) = block
                        .transactions
                        .iter()
                        .find(|t| t.txid().ok() == Some(tx_hash))
                    {
                        if let Some(output) = tx.outputs.get(outpoint.vout as usize) {
                            return Ok(Some(output.clone()));
                        }
                    }
                }
            }
        }

        Ok(None)
    }

    /// Compiles a complete transaction history for a given public key.
    /// It fetches transactions from both the database (confirmed) and the mempool (pending).
    pub fn get_wallet_transaction_history(
        &self,
        pubkey: &PublicKey,
    ) -> Result<Vec<WalletTransactionInfo>> {
        const MAX_HISTORY_ITEMS: usize = 1000;

        let mut transaction_history_info = Vec::new();
        let mut seen_tx_hashes = HashSet::new();

        // Get all transaction hashes associated with this public key from the `history_` index.
        let tx_hashes = self.get_transaction_hashes_by_pubkey_from_db(pubkey)?;

        // For each hash, get the full transaction details.
        for tx_hash in &tx_hashes {
            if seen_tx_hashes.insert(*tx_hash) && transaction_history_info.len() < MAX_HISTORY_ITEMS
            {
                if let Some((tx, block_index, timestamp)) =
                    self.get_transaction_with_details(&tx_hash)?
                {
                    let status = match block_index {
                        Some(index) => TransactionStatus::Confirmed {
                            block_hash: tx.txid()?,
                            block_index: index,
                        },
                        None => TransactionStatus::Pending,
                    };
                    transaction_history_info.push(WalletTransactionInfo {
                        transaction: tx,
                        status,
                        block_timestamp: Some(timestamp),
                        block_index,
                    });
                }
            }
        }

        // Also check the mempool for any relevant pending transactions.
        for (tx_hash, (timestamp, tx, _fee)) in self.mempool.iter() {
            let is_relevant = tx.outputs.iter().any(|o| o.pubkey == *pubkey)
                || tx.inputs.iter().any(|i| {
                    self.utxo_set
                        .utxos
                        .get(&i.outpoint)
                        .map_or(false, |(_, o)| o.pubkey == *pubkey)
                });

            if is_relevant
                && seen_tx_hashes.insert(*tx_hash)
                && transaction_history_info.len() < MAX_HISTORY_ITEMS
            {
                transaction_history_info.push(WalletTransactionInfo {
                    transaction: tx.clone(),
                    status: TransactionStatus::Pending,
                    block_timestamp: Some(*timestamp),
                    block_index: None,
                });
            }
        }

        transaction_history_info.truncate(MAX_HISTORY_ITEMS);

        // Sort the final list by timestamp, newest first.
        transaction_history_info.sort_by(|a, b| {
            b.block_timestamp
                .unwrap_or_default()
                .cmp(&a.block_timestamp.unwrap_or_default())
        });

        Ok(transaction_history_info)
    }
}
