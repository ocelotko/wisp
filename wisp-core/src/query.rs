use crate::blockchain::Blockchain;
use anyhow::{anyhow, Context, Result};
use bincode::config::standard as bincode_config;
use chrono::{DateTime, Utc};
use log::{debug, error};
use std::collections::{HashMap, HashSet};

use crate::{
    network::{TransactionStatus, WalletTransactionInfo},
    sha256::Hash,
    signatures::PublicKey,
    storage::DBKeys,
    transactions::{OutPoint, Transaction, TransactionOutput},
};

impl Blockchain {
    /// Gets all UTXOs belonging to a specific public key from the in-memory UTXO set.
    /// This is now mempool-aware and will not include UTXOs that are spent by transactions
    /// currently in the mempool.
    pub fn get_utxos_for_pubkey(&self, pubkey: &PublicKey) -> Vec<(OutPoint, TransactionOutput)> {
        self.utxo_set
            .utxos
            .iter()
            .filter_map(|(outpoint, output)| {
                if output.pubkey == *pubkey && !self.mempool_spent_utxos.contains(outpoint) {
                    Some((*outpoint, output.clone()))
                } else {
                    None
                }
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

        let key = DBKeys::tx_location(tx_hash);
        match self.db.get(&key) {
            Ok(Some(ivec)) => {
                if ivec.len() != 8 {
                    error!(
                        "Invalid tx_location length for {}: {} (expected 8). Treating as not found.",
                        tx_hash,
                        ivec.len()
                    );
                    return TransactionStatus::NotFound;
                }
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&ivec);
                let block_index = u64::from_be_bytes(bytes);

                match self.get_block_by_index(block_index) {
                    Ok(Some(block)) => match block.id() {
                        Ok(block_hash) => TransactionStatus::Confirmed {
                            block_hash,
                            block_index,
                        },
                        Err(e) => {
                            error!("Failed to hash block {} during status check, which may indicate data corruption: {}", block.index, e);
                            TransactionStatus::NotFound
                        }
                    },
                    Ok(None) => TransactionStatus::NotFound,
                    Err(e) => {
                        error!(
                            "DB error fetching block {} for status check: {}",
                            block_index, e
                        );
                        TransactionStatus::NotFound
                    }
                }
            }
            Ok(None) => {
                debug!(
                    "Transaction {} not found in mempool or confirmed blocks.",
                    tx_hash
                );
                TransactionStatus::NotFound
            }
            Err(e) => {
                error!("Database error checking tx_location for {}: {}", tx_hash, e);
                TransactionStatus::NotFound
            }
        }
    }

    /// Retrieves a transaction and its associated metadata (block height, timestamp).
    /// It first checks the mempool, then falls back to the database.
    pub fn get_transaction_with_details(
        &self,
        tx_hash: &Hash,
    ) -> Result<Option<(Transaction, Option<u64>, DateTime<Utc>)>> {
        // Check the mempool first for unconfirmed transactions.
        if let Some(entry) = self.mempool.get(tx_hash) {
            return Ok(Some((entry.transaction.clone(), None, entry.timestamp)));
        }

        // If not in mempool, check the database for a confirmed transaction.
        let key = DBKeys::tx_location(tx_hash);
        if let Some(ivec) = self.db.get(&key)? {
            if ivec.len() != 8 {
                error!(
                    "Invalid tx_location length for {}: {} (expected 8). Treating as not found.",
                    tx_hash,
                    ivec.len()
                );
                return Ok(None);
            }
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
                } else {
                    // This indicates a DB inconsistency.
                    log::warn!("Transaction {} not found in block {} despite tx_location entry pointing to it.", tx_hash, block_index);
                    return Ok(None);
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
        if let Some(output) = self.utxo_set.utxos.get(outpoint) {
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

    /// Finds multiple transaction outputs by their `OutPoint`s.
    /// It checks the live UTXO set first, then falls back to searching the database for historical transactions.
    /// This is more efficient than calling `find_output_by_outpoint_in_chain_or_utxos` in a loop.
    pub fn find_outputs_by_outpoints(
        &self,
        outpoints: &[OutPoint],
    ) -> Result<HashMap<OutPoint, TransactionOutput>> {
        let mut results = HashMap::with_capacity(outpoints.len());
        let mut needed_from_db = Vec::new();

        // First, check the in-memory UTXO set.
        for outpoint in outpoints {
            if let Some(output) = self.utxo_set.utxos.get(outpoint) {
                results.insert(*outpoint, output.clone());
            } else {
                needed_from_db.push(*outpoint);
            }
        }

        // For any not found in memory, check the database.
        for outpoint in needed_from_db {
            if let Some(output) = self.find_output_by_outpoint_in_db(&outpoint)? {
                results.insert(outpoint, output);
            }
        }

        Ok(results)
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
        if let Some(ivec) = db.get(DBKeys::tx_location(&tx_hash))? {
            if ivec.len() != 8 {
                error!(
                    "Invalid tx_location length for {}: {} (expected 8). Treating as not found.",
                    tx_hash,
                    ivec.len()
                );
                return Ok(None);
            }
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&ivec);
            let block_index = u64::from_be_bytes(bytes);

            // Nested logic to get block from index
            if let Some(hash_ivec) = db.get(DBKeys::index_to_hash(block_index))? {
                let (hash, _): (Hash, _) =
                    bincode::decode_from_slice(&hash_ivec, bincode_config())?;

                // Nested logic to get block from hash
                if let Some(block_ivec) = db.get(DBKeys::block(&hash))? {
                    let (checked_block, _): (crate::blockchain::CheckedBlock, _) =
                        bincode::decode_from_slice(&block_ivec, bincode_config())
                            .context("Failed to decode CheckedBlock in find_output_by_outpoint")?;
                    let block = checked_block.into_block().context(
                        "Failed to verify and unwrap CheckedBlock in find_output_by_outpoint",
                    )?;
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
                    let status = if let Some(index) = block_index {
                        // Correctly fetch the block hash for the confirmed transaction.
                        let block_hash = self
                            .get_block_by_index(index)?
                            .ok_or_else(|| {
                                anyhow!(
                                    "Block {} not found for wallet history, but tx_location exists",
                                    index
                                )
                            })?
                            .id()?;
                        TransactionStatus::Confirmed {
                            block_hash,
                            block_index: index,
                        }
                    } else {
                        TransactionStatus::Pending
                    };

                    transaction_history_info.push(WalletTransactionInfo {
                        transaction: tx,
                        // The status now correctly contains the block hash.
                        status,
                        block_timestamp: Some(timestamp),
                        block_index,
                    });
                }
            }
        }

        // Also check the mempool for any relevant pending transactions.
        for (tx_hash, entry) in self.mempool.iter() {
            let is_relevant = entry
                .transaction
                .outputs
                .iter()
                .any(|o| o.pubkey == *pubkey)
                || entry.transaction.inputs.iter().any(|i| {
                    self.utxo_set
                        .utxos
                        .get(&i.outpoint)
                        .map_or(false, |o| o.pubkey == *pubkey)
                });

            if is_relevant
                && seen_tx_hashes.insert(*tx_hash)
                && transaction_history_info.len() < MAX_HISTORY_ITEMS
            {
                transaction_history_info.push(WalletTransactionInfo {
                    transaction: entry.transaction.clone(),
                    status: TransactionStatus::Pending,
                    block_timestamp: Some(entry.timestamp),
                    block_index: None,
                });
            }
        }

        transaction_history_info.truncate(MAX_HISTORY_ITEMS);

        // Sort the final list by timestamp, newest first.
        // Using unwrap_or_default() for None timestamps will push pending transactions (which might have a recent timestamp)
        // or corrupted entries to the end of the list if their timestamp is older than confirmed ones. This is acceptable.
        transaction_history_info.sort_by(|a, b| {
            b.block_timestamp
                .unwrap_or_default()
                .cmp(&a.block_timestamp.unwrap_or_default())
        });

        Ok(transaction_history_info)
    }
}
