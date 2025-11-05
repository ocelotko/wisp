use crate::{
    blockchain::Block,
    transactions::{OutPoint, TransactionOutput},
};
use anyhow::{anyhow, Result};
use bincode::{Decode, Encode};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Encode, Decode, Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct UtxoSet {
    pub utxos: HashMap<OutPoint, TransactionOutput>,
}

impl UtxoSet {
    pub fn new() -> Self {
        UtxoSet {
            utxos: HashMap::new(),
        }
    }

    pub fn apply_block(&mut self, block: &Block) -> Result<()> {
        for transaction in &block.transactions {
            let txid = transaction.txid()?;
            if !transaction.is_coinbase() {
                for input in &transaction.inputs {
                    self.utxos.remove(&input.outpoint);
                }
            }
            for (vout, output) in transaction.outputs.iter().enumerate() {
                let outpoint = OutPoint {
                    txid,
                    vout: vout as u32,
                };
                self.utxos.insert(outpoint, output.clone());
            }
        }
        Ok(())
    }

    /// Creates a new `UtxoSet` that represents the state after applying a block, without modifying the original.
    /// This is used for creating pending snapshots atomically.
    pub fn simulate_apply(&self, block: &Block) -> Result<Self> {
        let mut new_set = self.clone();
        new_set.apply_block(block)?;
        Ok(new_set)
    }

    pub fn revert_block<F>(&mut self, block: &Block, mut find_spent_output: F) -> Result<()>
    where
        F: FnMut(&OutPoint) -> Result<Option<TransactionOutput>>,
    {
        // Iterate through transactions in reverse order to correctly handle dependencies.
        for transaction in block.transactions.iter().rev() {
            let txid = transaction.txid()?;

            // 1. Remove the outputs created by this transaction.
            // This makes them no longer spendable.
            for (vout, _) in transaction.outputs.iter().enumerate() {
                let outpoint = OutPoint {
                    txid,
                    vout: vout as u32,
                };
                self.utxos.remove(&outpoint);
            }

            // 2. Re-add the inputs that this transaction spent.
            // This makes the previously spent UTXOs available again.
            for input in &transaction.inputs {
                if let Some(spent_output) = find_spent_output(&input.outpoint)? {
                    // The boolean flag is gone
                    self.utxos.insert(input.outpoint, spent_output);
                }
            }
        }
        Ok(())
    }

    /// Performs a "dry run" validation of a block's transactions against the current UTXO set.
    /// This is a lightweight check that avoids cloning the entire UTXO set. It ensures that:
    /// 1. All transaction inputs exist in the UTXO set or are created within the block itself.
    /// 2. There are no double-spends within the block.
    pub fn validate_block_utxos(&self, block: &Block) -> Result<()> {
        let mut spent_in_block: HashSet<OutPoint> = HashSet::new();
        let mut created_in_block: HashMap<OutPoint, TransactionOutput> = HashMap::new();

        for tx in &block.transactions {
            let txid = tx.txid()?;

            // For non-coinbase transactions, validate inputs.
            if !tx.is_coinbase() {
                if tx.inputs.is_empty() {
                    return Err(anyhow!("Transaction {} has no inputs", txid));
                }

                for input in &tx.inputs {
                    // Check for intra-block double spend.
                    if !spent_in_block.insert(input.outpoint) {
                        return Err(anyhow!(
                            "Intra-block double spend detected for UTXO: {}",
                            input.outpoint
                        ));
                    }

                    // Check if the input exists, either from a previous transaction in this block
                    // or from the main UTXO set.
                    if !created_in_block.contains_key(&input.outpoint)
                        && !self.utxos.contains_key(&input.outpoint)
                    {
                        return Err(anyhow!(
                            "Input UTXO {} for tx {} not found in UTXO set or block",
                            input.outpoint,
                            txid
                        ));
                    }
                }
            }

            // Add outputs of the current transaction to a temporary map for subsequent transactions to reference.
            for (vout, output) in tx.outputs.iter().enumerate() {
                created_in_block.insert(
                    OutPoint {
                        txid,
                        vout: vout as u32,
                    },
                    output.clone(),
                );
            }
        }

        Ok(())
    }
}

impl Default for UtxoSet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        currency::Amount, sha256::Hash, signatures::PrivateKey, transactions::Transaction, utils,
    };
    use chrono::Utc;
    use std::collections::HashMap;

    // Helper to create a funded UTXO set for testing.
    fn create_funded_utxo_set(key: &PrivateKey, amounts: &[u64]) -> (UtxoSet, Vec<OutPoint>) {
        let mut utxos = HashMap::new();
        let mut outpoints = Vec::new();
        let pubkey = key.public_key();

        for (i, &amount) in amounts.iter().enumerate() {
            // Create a fake funding transaction for each UTXO.
            let funding_tx = Transaction::new(
                vec![],
                vec![TransactionOutput {
                    value: Amount::from_smallest_unit(amount),
                    pubkey,
                }],
            );
            let txid = funding_tx.txid().unwrap();
            let outpoint = OutPoint {
                txid,
                vout: i as u32,
            };
            utxos.insert(outpoint, funding_tx.outputs[0].clone());
            outpoints.push(outpoint);
        }

        (UtxoSet { utxos }, outpoints)
    }

    // Helper to create a simple block for testing.
    fn create_test_block(transactions: Vec<Transaction>) -> Block {
        Block {
            version: 1,
            timestamp: Utc::now(),
            nonce: 0,
            previous_hash: Hash::zero(),
            merkle_root: utils::MerkleRoot::calculate(&transactions).unwrap(),
            target: crate::MAX_TARGET,
            index: 1,
            transactions,
        }
    }

    #[test]
    fn test_apply_and_revert_block_roundtrip() {
        let key = PrivateKey::generate_keypair_with_rng(&mut ecdsa::signature::rand_core::OsRng);
        let (mut utxo_set, outpoints_to_spend) = create_funded_utxo_set(&key, &[1000, 500]);
        let original_utxo_set = utxo_set.clone();

        // Create a transaction that spends the first UTXO.
        let tx = Transaction::new_signed_from_utxos(
            &[outpoints_to_spend[0]],
            vec![TransactionOutput {
                value: Amount::from_smallest_unit(900),
                pubkey: key.public_key(),
            }],
            &key,
        )
        .unwrap();

        let block = create_test_block(vec![tx.clone()]);

        // Apply the block.
        utxo_set.apply_block(&block).unwrap();

        // The spent UTXO should be gone, and the new one should exist.
        assert!(!utxo_set.utxos.contains_key(&outpoints_to_spend[0]));
        assert!(utxo_set.utxos.contains_key(&OutPoint {
            txid: tx.txid().unwrap(),
            vout: 0
        }));
        // The unspent UTXO should still be there.
        assert!(utxo_set.utxos.contains_key(&outpoints_to_spend[1]));

        // Revert the block.
        // The `find_spent_output` closure simulates looking up the spent UTXO from the original state.
        utxo_set
            .revert_block(&block, |outpoint| {
                Ok(original_utxo_set.utxos.get(outpoint).cloned())
            })
            .unwrap();

        // The state should be identical to the original state.
        assert_eq!(utxo_set, original_utxo_set);
    }

    #[test]
    fn test_simulate_apply_does_not_mutate() {
        let key = PrivateKey::generate_keypair_with_rng(&mut ecdsa::signature::rand_core::OsRng);
        let (utxo_set, outpoints_to_spend) = create_funded_utxo_set(&key, &[1000]);
        let original_utxo_set_clone = utxo_set.clone();

        let tx = Transaction::new_signed_from_utxos(
            &[outpoints_to_spend[0]],
            vec![TransactionOutput {
                value: Amount::from_smallest_unit(900),
                pubkey: key.public_key(),
            }],
            &key,
        )
        .unwrap();
        let block = create_test_block(vec![tx]);

        // Simulate applying the block.
        let simulated_set = utxo_set.simulate_apply(&block).unwrap();

        // The original set should be unchanged.
        assert_eq!(utxo_set, original_utxo_set_clone);

        // The simulated set should reflect the changes.
        assert_ne!(simulated_set, utxo_set);
        assert!(!simulated_set.utxos.contains_key(&outpoints_to_spend[0]));
    }

    #[test]
    fn test_validate_block_utxos_detects_double_spend() {
        let key = PrivateKey::generate_keypair_with_rng(&mut ecdsa::signature::rand_core::OsRng);
        let (utxo_set, outpoints_to_spend) = create_funded_utxo_set(&key, &[1000]);

        // Create two transactions spending the same UTXO.
        let tx1 = Transaction::new_signed_from_utxos(
            &[outpoints_to_spend[0]],
            vec![TransactionOutput {
                value: Amount::from_smallest_unit(400),
                pubkey: key.public_key(),
            }],
            &key,
        )
        .unwrap();
        let tx2 = Transaction::new_signed_from_utxos(
            &[outpoints_to_spend[0]], // Same UTXO
            vec![TransactionOutput {
                value: Amount::from_smallest_unit(500),
                pubkey: key.public_key(),
            }],
            &key,
        )
        .unwrap();

        let block = create_test_block(vec![tx1, tx2]);

        // Validation should fail due to the intra-block double spend.
        let result = utxo_set.validate_block_utxos(&block);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Intra-block double spend detected"));
    }

    #[test]
    fn test_validate_block_utxos_with_coinbase() {
        let utxo_set = UtxoSet::new();
        let genesis_block = utils::genesis_block().unwrap();

        // The genesis block (a coinbase transaction) should be valid against an empty UTXO set.
        assert!(utxo_set.validate_block_utxos(&genesis_block).is_ok());
    }
}
