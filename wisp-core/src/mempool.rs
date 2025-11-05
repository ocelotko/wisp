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

/// Represents an entry in the mempool.
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
    /// Adds a transaction to the mempool after performing a series of validations.
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

    /// Clears the mempool of transactions that have exceeded the maximum age.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils;
    use crate::{signatures::PrivateKey, transactions::TransactionOutput};
    use tempfile::tempdir;

    // Helper to create a temporary DB and a Blockchain instance for testing.
    fn setup_test_blockchain() -> (Blockchain, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db = sled::open(dir.path()).unwrap();
        let blockchain = Blockchain::new(db);
        // For mempool tests, we don't need a full chain, just a UTXO set.
        // So we don't call `load_from_db` which would add a genesis block.
        // This keeps the tests focused on mempool logic.
        (blockchain, dir)
    }

    // Helper to create a funded UTXO for a given private key.
    fn fund_utxo(
        blockchain: &mut Blockchain,
        private_key: &PrivateKey,
        amount: Amount,
    ) -> OutPoint {
        // In a real scenario, this UTXO would come from a confirmed block.
        // For isolated mempool testing, we can manually insert it.
        let pubkey = private_key.public_key();
        // Create a fake transaction to be the source of the UTXO.
        let funding_tx = Transaction::new(
            vec![],
            vec![TransactionOutput {
                value: amount,
                pubkey,
            }],
        );
        let txid = funding_tx.txid().unwrap();
        let outpoint = OutPoint { txid, vout: 0 };

        // Manually insert the UTXO into the blockchain's UTXO set.
        // The `(false, ...)` tuple indicates it is not yet spent in the mempool.
        blockchain
            .utxo_set
            .utxos
            .insert(outpoint, funding_tx.outputs[0].clone());

        outpoint
    }

    #[test]
    fn test_add_valid_transaction_to_mempool() {
        let (mut blockchain, _dir) = setup_test_blockchain();
        let key = PrivateKey::generate_keypair_with_rng(&mut ecdsa::signature::rand_core::OsRng);
        let utxo_to_spend = fund_utxo(&mut blockchain, &key, Amount::from_smallest_unit(1000));

        let tx = Transaction::new_signed_from_utxos(
            &[utxo_to_spend],
            vec![TransactionOutput {
                value: Amount::from_smallest_unit(900),
                pubkey: key.public_key(), // Sending back to self for simplicity
            }],
            &key,
        )
        .unwrap();

        assert!(blockchain.add_to_mempool(tx.clone()).is_ok());
        assert_eq!(blockchain.mempool.len(), 1);
        assert!(blockchain.mempool.contains_key(&tx.txid().unwrap()));
        // Check that the UTXO is now marked as spent in the separate mempool set
        assert!(blockchain.mempool_spent_utxos.contains(&utxo_to_spend));
    }

    #[test]
    fn test_reject_mempool_double_spend() {
        let (mut blockchain, _dir) = setup_test_blockchain();
        let key = PrivateKey::generate_keypair_with_rng(&mut ecdsa::signature::rand_core::OsRng);
        let utxo_to_spend = fund_utxo(&mut blockchain, &key, Amount::from_smallest_unit(1000));

        // First transaction, valid
        let tx1 = Transaction::new_signed_from_utxos(
            &[utxo_to_spend],
            vec![TransactionOutput {
                value: Amount::from_smallest_unit(500),
                pubkey: key.public_key(),
            }],
            &key,
        )
        .unwrap();
        assert!(blockchain.add_to_mempool(tx1).is_ok());

        // Second transaction, attempts to spend the same UTXO
        let tx2 = Transaction::new_signed_from_utxos(
            &[utxo_to_spend],
            vec![TransactionOutput {
                value: Amount::from_smallest_unit(400),
                pubkey: key.public_key(),
            }],
            &key,
        )
        .unwrap();

        let result = blockchain.add_to_mempool(tx2);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("already spent by a transaction in mempool"));
        assert_eq!(blockchain.mempool.len(), 1); // Only the first tx should be in the mempool
    }

    #[test]
    fn test_reject_insufficient_funds() {
        let (mut blockchain, _dir) = setup_test_blockchain();
        let key = PrivateKey::generate_keypair_with_rng(&mut ecdsa::signature::rand_core::OsRng);
        let utxo_to_spend = fund_utxo(&mut blockchain, &key, Amount::from_smallest_unit(1000));

        // Try to spend more than we have
        let tx = Transaction::new_signed_from_utxos(
            &[utxo_to_spend],
            vec![TransactionOutput {
                value: Amount::from_smallest_unit(1001), // More than the input
                pubkey: key.public_key(),
            }],
            &key,
        )
        .unwrap();

        let result = blockchain.add_to_mempool(tx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("insufficient funds"));
    }

    #[test]
    fn test_reject_coinbase_in_mempool() {
        let (mut blockchain, _dir) = setup_test_blockchain();
        let coinbase_tx = utils::genesis_block().unwrap().transactions[0].clone();
        let result = blockchain.add_to_mempool(coinbase_tx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Coinbase transaction cannot be added"));
    }
}
