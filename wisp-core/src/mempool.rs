use crate::{
    blockchain::{Block, Blockchain},
    currency::Amount,
    sha256::Hash,
    transactions::{OutPoint, Transaction},
};

use anyhow::{anyhow, Result};
use chrono::{Duration as ChronoDuration, Utc};
use ecdsa::signature::Verifier;
use log::{debug, warn};
use std::collections::HashSet;

/// The maximum age in seconds for a transaction to remain in the mempool before being evicted.
pub const MAX_MEMPOOL_TRANSACTION_AGE: u64 = 172800; // Two days in seconds

impl Blockchain {
    /// Adds a transaction to the mempool after performing a series of validations.
    pub fn add_to_mempool(&mut self, transaction: Transaction) -> Result<()> {
        // Coinbase transactions are only allowed in blocks, not in the mempool.
        if transaction.inputs.is_empty() {
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
            output_sum = (output_sum + output.value)?;
        }

        let transaction_hash_for_verification = transaction.txid()?;
        for input in &transaction.inputs {
            let outpoint = &input.outpoint;

            // Check for duplicate inputs within the same transaction.
            if !inputs_checked_in_tx.insert(*outpoint) {
                return Err(anyhow!("Transaction has duplicate inputs: {}", outpoint));
            }

            // Check if the input UTXO exists and is not already spent by another mempool transaction.
            // The boolean in the UTXO entry indicates if it's "spent" in the mempool.
            let (is_in_mempool, prev_output) = match self.utxo_set.utxos.get(outpoint) {
                Some((is_in_mempool, output)) => (*is_in_mempool, output),
                None => {
                    return Err(anyhow!(
                        "Transaction input UTXO {} not found or already spent on chain",
                        outpoint
                    ));
                }
            };

            if is_in_mempool {
                return Err(anyhow!(
                    "Double spend: input {} already spent by a transaction in mempool",
                    outpoint
                ));
            }

            // Verify the signature for the input.
            let is_signature_valid = match input.signature.as_ref() {
                Some(sig) => prev_output
                    .pubkey
                    .0
                    .verify(&transaction_hash_for_verification.as_bytes(), sig)
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

            input_sum = (input_sum + prev_output.value)?;
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
            if let Some(utxo_entry) = self.utxo_set.utxos.get_mut(&outpoint) {
                utxo_entry.0 = true;
            } else {
                return Err(anyhow!("CRITICAL: UTXO {} disappeared between validation and marking. Aborting mempool add.", outpoint));
            }
        }

        // Add the validated transaction to the mempool.
        let fee = (input_sum - output_sum)?;
        self.mempool
            .insert(tx_hash, (Utc::now(), transaction.clone(), fee));

        println!(
            "Transaction added to mempool. Mempool size: {}",
            self.mempool.len()
        );
        Ok(())
    }

    /// Clears the mempool of transactions that have exceeded the maximum age.
    pub fn clear_mempool(&mut self) {
        let now = Utc::now();
        let mut outpoints_to_unmark: Vec<OutPoint> = vec![];

        // Retain only transactions that are not too old.
        self.mempool.retain(|_, (timestamp, transaction, _)| {
            let is_too_old = now.signed_duration_since(*timestamp)
                > ChronoDuration::seconds(MAX_MEMPOOL_TRANSACTION_AGE as i64);
            if is_too_old {
                outpoints_to_unmark.extend(transaction.inputs.iter().map(|input| input.outpoint));
            }
            !is_too_old
        });

        // Unmark the UTXOs that were spent by the expired transactions.
        for outpoint in outpoints_to_unmark {
            if let Some((marked, _)) = self.utxo_set.utxos.get_mut(&outpoint) {
                *marked = false;
            }
        }
    }

    /// Removes transactions from the mempool that have been included in a new block.
    pub fn clear_mempool_of_block_transactions(&mut self, block: &Block) {
        // Create a set of transaction hashes from the block for efficient lookup.
        let mut block_transaction_hashes: HashSet<Hash> = HashSet::new();
        for tx in &block.transactions {
            if let Ok(hash) = tx.txid() {
                block_transaction_hashes.insert(hash);
            } else {
                warn!("Failed to hash transaction for mempool clearing.");
            }
        }

        let initial_mempool_size = self.mempool.len();
        // Retain only the transactions that are NOT in the new block.
        self.mempool
            .retain(|hash, _| !block_transaction_hashes.contains(hash));

        debug!(
            "Removed {} transactions from mempool for block {}.",
            initial_mempool_size - self.mempool.len(),
            block.id().unwrap_or_default()
        );
    }
}
