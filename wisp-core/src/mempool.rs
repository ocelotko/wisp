use crate::{
    blockchain::{Block, Blockchain},
    currency::Amount,
    sha256::Hash,
    transactions::{OutPoint, Transaction},
};

use anyhow::{anyhow, Context, Result};
use bincode::{Decode, Encode};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ecdsa::signature::Verifier;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Represents a transaction entry in the mempool.
///
/// Each entry contains the transaction itself, the time it was added,
/// and its pre-calculated fee to facilitate sorting and prioritization.
#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MempoolEntry {
    /// The time the transaction was added to the mempool.
    #[bincode(with_serde)]
    pub timestamp: DateTime<Utc>,
    /// The transaction itself.
    pub transaction: Transaction,
    /// The pre-calculated fee for the transaction.
    pub fee: Amount,
}

/// The maximum age in seconds for a transaction to remain in the mempool before being evicted.
pub const MAX_MEMPOOL_TRANSACTION_AGE: u64 = 172800; // Two days in seconds

impl Blockchain {
    /// Adds a transaction to the mempool after performing a series of validation checks.
    ///
    /// The validation includes checks for coinbase transactions, double-spends against
    /// both the UTXO set and other mempool transactions, signature validity, and fund sufficiency.
    pub fn add_to_mempool(&mut self, transaction: Transaction) -> Result<()> {
        // Use the canonical check for a coinbase transaction.
        if transaction.is_coinbase() {
            return Err(anyhow!("Coinbase transaction cannot be added to mempool"));
        }

        let tx_hash = transaction.txid()?;

        // Check if the transaction is already in the mempool.
        if self.mempool.contains_key(&tx_hash) {
            debug!(
                "Transaction {} already exists in mempool. Ignoring.",
                tx_hash
            );
            return Ok(());
        }

        // --- Begin Transaction Validation ---
        let mut input_sum = Amount::zero();
        let mut inputs_checked_in_tx: HashSet<OutPoint> = HashSet::new();

        let mut output_sum = Amount::zero();
        for output in &transaction.outputs {
            output_sum = output_sum
                .checked_add(output.value)
                .context("Output sum overflow")?;
        }

        let transaction_hash_for_verification = transaction.txid()?;
        for input in &transaction.inputs {
            let outpoint = &input.outpoint;

            // Check for duplicate inputs within the same transaction.
            if !inputs_checked_in_tx.insert(*outpoint) {
                return Err(anyhow!("Transaction has duplicate inputs: {}", outpoint));
            }

            // Check if the input UTXO exists and is not already spent by another mempool transaction.
            if self.mempool_spent_utxos.contains(outpoint) {
                return Err(anyhow!(
                    "Double spend: input {} already spent by a transaction in mempool",
                    outpoint
                ));
            }

            let prev_output = match self.utxo_set.utxos.get(outpoint) {
                Some(output) => output,
                None => {
                    return Err(anyhow!(
                        "Transaction input UTXO {} not found or already spent on chain",
                        outpoint
                    ));
                }
            };

            // Verify the signature for the input.
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

        // Ensure that the total input value is not less than the total output value.
        if input_sum < output_sum {
            return Err(anyhow!(
                "Invalid transaction {}: inputs ({}) < outputs ({}) (insufficient funds)",
                transaction.txid()?,
                input_sum,
                output_sum
            ));
        }

        // Mark the UTXOs used by this transaction as "spent" in the mempool.
        let outpoints_to_mark: Vec<OutPoint> =
            transaction.inputs.iter().map(|i| i.outpoint).collect();

        for outpoint in outpoints_to_mark {
            self.mempool_spent_utxos.insert(outpoint);
        }

        // Add the validated transaction to the mempool.
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
            },
        );

        println!(
            "Transaction {} added to mempool with fee {}. Mempool size: {}",
            tx_hash,
            fee,
            self.mempool.len()
        );

        // Persist the updated mempool to disk.
        if let Err(e) = self.save_mempool_snapshot() {
            warn!("[MEMPOOL] Failed to save mempool snapshot: {}", e);
        }
        Ok(())
    }

    /// Evicts transactions from the mempool that have exceeded the maximum age.
    pub fn clear_mempool(&mut self) {
        let now = Utc::now();
        let mut outpoints_to_unmark: Vec<OutPoint> = vec![];

        // Retain only transactions that are not too old.
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

        // Unmark the UTXOs that were spent by the expired transactions.
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

    /// Removes transactions from the mempool that have been included in a new block.
    pub fn clear_mempool_of_block_transactions(&mut self, block: &Block, block_hash: Hash) {
        let txids_in_block: HashSet<Hash> = block
            .transactions
            .iter()
            .filter_map(|tx| tx.txid().ok())
            .collect();

        let initial_mempool_size = self.mempool.len();

        let mut outpoints_to_unmark = Vec::new();
        for (txid, entry) in self.mempool.iter() {
            if txids_in_block.contains(txid) {
                outpoints_to_unmark.extend(entry.transaction.inputs.iter().map(|i| i.outpoint));
            }
        }

        // Retain only the transactions that are NOT in the new block.
        self.mempool
            .retain(|txid, _| !txids_in_block.contains(txid));

        for outpoint in outpoints_to_unmark {
            self.mempool_spent_utxos.remove(&outpoint);
        }
        let removed_count = initial_mempool_size - self.mempool.len();

        // Persist the change if any transactions were removed.
        if removed_count > 0 {
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
