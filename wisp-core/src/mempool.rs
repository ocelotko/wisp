use crate::{
    blockchain::{Block, Blockchain},
    currency::Amount,
    sha256::Hash,
    transactions::{OutPoint, Transaction},
};

use anyhow::{anyhow, Context, Result};
use bincode::{config::standard as bincode_config, Decode, Encode};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ecdsa::signature::Verifier;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Represents a transaction entry in the mempool.
#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MempoolEntry {
    #[bincode(with_serde)]
    pub timestamp: DateTime<Utc>,
    pub transaction: Transaction,
    pub fee: Amount,
    pub serialized_size: usize,
}

pub const MAX_MEMPOOL_TRANSACTION_AGE: u64 = 172800; // Two days in seconds

impl Blockchain {
    pub fn add_to_mempool(&mut self, transaction: Transaction) -> Result<()> {
        self.add_transaction_to_mempool(transaction, false)
    }

    pub(crate) fn add_transaction_to_mempool(
        &mut self,
        transaction: Transaction,
        persist: bool,
    ) -> Result<()> {
        if transaction.is_coinbase() {
            return Err(anyhow!("Coinbase transaction cannot be added to mempool"));
        }

        let tx_hash = transaction.txid()?;
        let serialized_size = bincode::encode_to_vec(&transaction, bincode_config())?.len();

        if serialized_size > crate::MAX_TRANSACTION_SIZE_BYTES {
            return Err(anyhow!(
                "Transaction size ({} bytes) exceeds maximum allowed size ({} bytes)",
                serialized_size,
                crate::MAX_TRANSACTION_SIZE_BYTES
            ));
        }

        if self.mempool.contains_key(&tx_hash) {
            debug!(
                "Transaction {} already exists in mempool. Ignoring.",
                tx_hash
            );
            return Ok(());
        }

        let mut input_sum = Amount::zero();
        let mut inputs_checked_in_tx: HashSet<OutPoint> = HashSet::new();

        let mut output_sum = Amount::zero();
        for output in &transaction.outputs {
            if output.value.as_smallest_unit() < crate::MIN_OUTPUT_VALUE {
                return Err(anyhow!(
                    "Transaction output value {} is below the dust limit of {}",
                    output.value,
                    crate::MIN_OUTPUT_VALUE
                ));
            }
            output_sum = output_sum
                .checked_add(output.value)
                .context("Output sum overflow")?;
        }

        let transaction_hash_for_verification = transaction.txid()?;
        for input in &transaction.inputs {
            let outpoint = &input.outpoint;

            if !inputs_checked_in_tx.insert(*outpoint) {
                return Err(anyhow!("Transaction has duplicate inputs: {}", outpoint));
            }

            if self.mempool_spent_utxos.contains(outpoint) {
                return Err(anyhow!(
                    "Double spend: input {} already spent by a transaction in mempool",
                    outpoint
                ));
            }

            let prev_output = if let Some(output) = self.utxo_set.utxos.get(outpoint) {
                output.clone()
            } else if let Some(parent_entry) = self.mempool.get(&outpoint.txid) {
                if let Some(output) = parent_entry.transaction.outputs.get(outpoint.vout as usize) {
                    output.clone()
                } else {
                    return Err(anyhow!(
                        "Transaction input {} references non-existent output in mempool tx {}",
                        outpoint,
                        outpoint.txid
                    ));
                }
            } else {
                return Err(anyhow!(
                    "Transaction input UTXO {} not found or already spent on chain",
                    outpoint
                ));
            };

            let is_signature_valid = match input.signature.as_ref() {
                Some(sig) => prev_output
                    .pubkey
                    .0
                    .verify(&transaction_hash_for_verification.as_bytes(), &sig.0)
                    .is_ok(),
                None => false,
            };

            if !is_signature_valid {
                let sig_status = if input.signature.is_none() {
                    "missing"
                } else {
                    "invalid"
                };
                return Err(anyhow!(
                    "Transaction signature {} for input {} in transaction {}",
                    sig_status,
                    outpoint,
                    transaction.txid()?
                ));
            }

            input_sum = input_sum
                .checked_add(prev_output.value)
                .context("Input sum overflow")?;
        }

        if input_sum < output_sum {
            return Err(anyhow!(
                "Invalid transaction {}: inputs ({}) < outputs ({}) (insufficient funds)",
                transaction.txid()?,
                input_sum,
                output_sum
            ));
        }

        let outpoints_to_mark: Vec<OutPoint> =
            transaction.inputs.iter().map(|i| i.outpoint).collect();

        for outpoint in outpoints_to_mark {
            self.mempool_spent_utxos.insert(outpoint);
        }

        let fee = input_sum
            .checked_sub(output_sum)
            .context("Fee calculation underflow")?;
        let now = Utc::now();
        self.mempool.insert(
            tx_hash,
            MempoolEntry {
                timestamp: now,
                transaction: transaction.clone(),
                fee,
                serialized_size,
            },
        );

        info!(
            "Transaction {} added to mempool with fee {}. Mempool size: {}",
            tx_hash,
            fee,
            self.mempool.len()
        );

        if persist {
            if let Err(e) = self.save_mempool_snapshot() {
                warn!("[MEMPOOL] Failed to save mempool snapshot: {}", e);
            }
        }
        Ok(())
    }

    pub fn clear_mempool(&mut self) {
        let now = Utc::now();
        let mut outpoints_to_unmark: Vec<OutPoint> = vec![];

        self.mempool.retain(|_, entry| {
            let is_too_old = now.signed_duration_since(entry.timestamp)
                > ChronoDuration::seconds(
                    i64::try_from(MAX_MEMPOOL_TRANSACTION_AGE).unwrap_or(i64::MAX),
                );
            if is_too_old {
                outpoints_to_unmark
                    .extend(entry.transaction.inputs.iter().map(|input| input.outpoint));
            }
            !is_too_old
        });

        for outpoint in &outpoints_to_unmark {
            self.mempool_spent_utxos.remove(outpoint);
        }

        if !outpoints_to_unmark.is_empty() {
            if let Err(e) = self.save_mempool_snapshot() {
                warn!(
                    "[MEMPOOL] Failed to save mempool snapshot after clearing old transactions: {}",
                    e
                );
            }
        }
    }

    pub fn clear_mempool_of_block_transactions(&mut self, block: &Block, block_hash: Hash) {
        self.clear_mempool_of_block_transactions_internal(block, block_hash, true);
    }

    pub(crate) fn clear_mempool_of_block_transactions_internal(
        &mut self,
        block: &Block,
        block_hash: Hash,
        persist: bool,
    ) {
        let txids_in_block: HashSet<Hash> = block
            .transactions
            .iter()
            .filter_map(|tx| tx.txid().ok())
            .collect();

        let inputs_spent_by_block: HashSet<OutPoint> = block
            .transactions
            .iter()
            .flat_map(|tx| tx.inputs.iter().map(|i| i.outpoint))
            .collect();

        let initial_mempool_size = self.mempool.len();

        let mut outpoints_to_unmark = Vec::new();
        let mut txs_to_remove = Vec::new();

        for (txid, entry) in self.mempool.iter() {
            if txids_in_block.contains(txid) {
                txs_to_remove.push(*txid);
                outpoints_to_unmark.extend(entry.transaction.inputs.iter().map(|i| i.outpoint));
            } else {
                // Check for conflicts (double spends)
                for input in &entry.transaction.inputs {
                    if inputs_spent_by_block.contains(&input.outpoint) {
                        txs_to_remove.push(*txid);
                        outpoints_to_unmark
                            .extend(entry.transaction.inputs.iter().map(|i| i.outpoint));
                        break;
                    }
                }
            }
        }

        for txid in txs_to_remove {
            self.mempool.remove(&txid);
        }

        for outpoint in outpoints_to_unmark {
            self.mempool_spent_utxos.remove(&outpoint);
        }
        let removed_count = initial_mempool_size - self.mempool.len();

        if removed_count > 0 && persist {
            if let Err(e) = self.save_mempool_snapshot() {
                warn!("[MEMPOOL] Failed to save mempool snapshot after clearing block transactions: {}", e);
            }
        }

        info!(
            "[MEMPOOL] Removed {} transactions from mempool for block {}.",
            removed_count, block_hash
        );
    }
}
