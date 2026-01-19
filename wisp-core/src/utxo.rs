use crate::{
    blockchain::Block,
    transactions::{OutPoint, TransactionOutput},
};
use anyhow::{anyhow, Result};
use bincode::{Decode, Encode};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Represents the set of all Unspent Transaction Outputs (UTXOs) in the blockchain.
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

    pub fn simulate_apply(&self, block: &Block) -> Result<Self> {
        let mut new_set = self.clone();
        new_set.apply_block(block)?;
        Ok(new_set)
    }

    pub fn revert_block<F>(&mut self, block: &Block, mut find_spent_output: F) -> Result<()>
    where
        F: FnMut(&OutPoint) -> Result<Option<TransactionOutput>>,
    {
        for transaction in block.transactions.iter().rev() {
            let txid = transaction.txid()?;

            for (vout, _) in transaction.outputs.iter().enumerate() {
                let outpoint = OutPoint {
                    txid,
                    vout: vout as u32,
                };
                self.utxos.remove(&outpoint);
            }

            for input in &transaction.inputs {
                if let Some(spent_output) = find_spent_output(&input.outpoint)? {
                    self.utxos.insert(input.outpoint, spent_output);
                }
            }
        }
        Ok(())
    }

    pub fn validate_block_utxos(&self, block: &Block) -> Result<()> {
        let mut spent_in_block: HashSet<OutPoint> = HashSet::new();
        let mut created_in_block: HashMap<OutPoint, TransactionOutput> = HashMap::new();

        for tx in &block.transactions {
            let txid = tx.txid()?;

            if !tx.is_coinbase() {
                if tx.inputs.is_empty() {
                    return Err(anyhow!("Transaction {} has no inputs", txid));
                }

                for input in &tx.inputs {
                    if !spent_in_block.insert(input.outpoint) {
                        return Err(anyhow!(
                            "Intra-block double spend detected for UTXO: {}",
                            input.outpoint
                        ));
                    }

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
