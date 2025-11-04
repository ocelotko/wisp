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
    pub utxos: HashMap<OutPoint, (bool, TransactionOutput)>,
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
                self.utxos.insert(outpoint, (false, output.clone()));
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
                    self.utxos.insert(input.outpoint, (false, spent_output));
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
