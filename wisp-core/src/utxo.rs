use crate::{
    blockchain::Block,
    transactions::{OutPoint, TransactionOutput},
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Serialize, Deserialize, Debug)]
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
}
