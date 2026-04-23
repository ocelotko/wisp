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
    pub fn get_utxos_for_pubkey(
        &self,
        pubkey: &PublicKey,
        known_script_hashes: &HashSet<Hash>,
    ) -> Vec<(OutPoint, TransactionOutput)> {
        let mut outpoints = self
            .get_utxo_outpoints_by_address_id(&pubkey.fingerprint())
            .unwrap_or_default();

        for script_hash in known_script_hashes {
            if let Ok(list) = self.get_utxo_outpoints_by_address_id(&script_hash.to_string()) {
                outpoints.extend(list);
            }
        }

        outpoints
            .into_iter()
            .filter(|op| !self.mempool_spent_utxos.contains(op))
            .filter_map(|op| {
                self.utxo_set
                    .utxos
                    .get(&op)
                    .map(|output| (op, output.clone()))
            })
            .collect()
    }

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

    pub fn get_transaction_with_details(
        &self,
        tx_hash: &Hash,
    ) -> Result<Option<(Transaction, Option<u64>, DateTime<Utc>)>> {
        if let Some(entry) = self.mempool.get(tx_hash) {
            return Ok(Some((entry.transaction.clone(), None, entry.timestamp)));
        }

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
                    return Ok(Some((
                        tx.clone(),
                        Some(block.index),
                        block.header.timestamp,
                    )));
                } else {
                    log::warn!("Transaction {} not found in block {} despite tx_location entry pointing to it.", tx_hash, block_index);
                    return Ok(None);
                }
            }
        }

        Ok(None)
    }

    pub fn get_total_transaction_count(&self) -> Result<u64> {
        self.get_total_transaction_count_from_db()
    }

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

    pub fn find_outputs_by_outpoints(
        &self,
        outpoints: &[OutPoint],
    ) -> Result<HashMap<OutPoint, TransactionOutput>> {
        let mut results = HashMap::with_capacity(outpoints.len());
        let mut needed_from_db = Vec::new();

        for outpoint in outpoints {
            if let Some(output) = self.utxo_set.utxos.get(outpoint) {
                results.insert(*outpoint, output.clone());
            } else {
                needed_from_db.push(*outpoint);
            }
        }

        for outpoint in needed_from_db {
            if let Some(output) = self.find_output_by_outpoint_in_db(&outpoint)? {
                results.insert(outpoint, output);
            }
        }

        Ok(results)
    }

    pub fn find_output_by_outpoint_in_db(
        &self,
        outpoint: &OutPoint,
    ) -> Result<Option<TransactionOutput>> {
        Self::find_output_by_outpoint_in_db_static(&self.db, outpoint)
    }

    pub fn find_output_by_outpoint_in_db_static(
        db: &sled::Db,
        outpoint: &OutPoint,
    ) -> Result<Option<TransactionOutput>> {
        let tx_hash = outpoint.txid;

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

            if let Some(hash_ivec) = db.get(DBKeys::index_to_hash(block_index))? {
                let (hash, _): (Hash, _) =
                    bincode::decode_from_slice(&hash_ivec, bincode_config())?;

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

    pub fn get_wallet_transaction_history(
        &self,
        pubkey: &PublicKey,
        known_script_hashes: &HashSet<Hash>,
    ) -> Result<Vec<WalletTransactionInfo>> {
        const MAX_HISTORY_ITEMS: usize = 1000;

        let mut transaction_history_info = Vec::new();
        let mut seen_tx_hashes = HashSet::new();
        let pk_hash_bytes = crate::address::Address::hash160(pubkey);

        let tx_hashes = self.get_transaction_hashes_by_pubkey_from_db(pubkey)?;
        let mut tx_hashes = tx_hashes;
        for script_hash in known_script_hashes {
            tx_hashes.extend(self.get_transaction_hashes_by_hash_from_db(script_hash)?);
        }

        for tx_hash in &tx_hashes {
            if seen_tx_hashes.insert(*tx_hash) && transaction_history_info.len() < MAX_HISTORY_ITEMS
            {
                if let Some((tx, block_index, timestamp)) =
                    self.get_transaction_with_details(&tx_hash)?
                {
                    let status = if let Some(index) = block_index {
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
                        status,
                        block_timestamp: Some(timestamp),
                        block_index,
                    });
                }
            }
        }

        for (tx_hash, entry) in self.mempool.iter() {
            let is_relevant = entry.transaction.outputs.iter().any(|output| {
                output
                    .script
                    .is_relevant_to(pubkey, &pk_hash_bytes, known_script_hashes)
            }) || entry.transaction.inputs.iter().any(|i| {
                // Check if the input is spending a relevant UTXO
                self.utxo_set
                    .utxos
                    .get(&i.outpoint)
                    .map_or(false, |output| {
                        output
                            .script
                            .is_relevant_to(pubkey, &pk_hash_bytes, known_script_hashes)
                    })
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
        transaction_history_info.sort_by(|a, b| {
            b.block_timestamp
                .unwrap_or_default()
                .cmp(&a.block_timestamp.unwrap_or_default())
        });

        Ok(transaction_history_info)
    }
}
