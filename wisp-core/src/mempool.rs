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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{signatures::PrivateKey, transactions::TransactionOutput};
    use tempfile::tempdir;

    // Helper to create a temporary DB and a Blockchain instance for testing.
    fn setup_test_blockchain() -> (Blockchain, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db = sled::open(dir.path()).unwrap();
        let blockchain = Blockchain::new(db);
        (blockchain, dir)
    }

    // Helper to create a funded UTXO for a given private key.
    fn fund_utxo(
        blockchain: &mut Blockchain,
        private_key: &PrivateKey,
        amount: Amount,
    ) -> OutPoint {
        let pubkey = private_key.public_key();
        let tx = Transaction::new(
            vec![], // Dummy tx, not a real coinbase
            vec![TransactionOutput {
                value: amount,
                pubkey,
            }],
        );
        let txid = tx.txid().unwrap();
        let outpoint = OutPoint { txid, vout: 0 };

        // Manually insert into the UTXO set for testing purposes.
        blockchain
            .utxo_set
            .utxos
            .insert(outpoint, (false, tx.outputs[0].clone()));
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
        // Check that the UTXO is now marked as spent in the mempool
        assert_eq!(
            blockchain.utxo_set.utxos.get(&utxo_to_spend).unwrap().0,
            true
        );
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
        let coinbase_tx = crate::utils::genesis_block().unwrap().transactions[0].clone();
        let result = blockchain.add_to_mempool(coinbase_tx);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Coinbase transaction cannot be added"));
    }
}
