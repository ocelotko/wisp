use crate::{
    blockchain::{Block, Blockchain},
    currency::Amount,
    transactions::{OutPoint, TransactionOutput},
    utils::calculate_block_reward,
    utils::MerkleRoot,
    U256,
};

use anyhow::{anyhow, Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use ecdsa::signature::Verifier;
use log::{debug, info};
use std::collections::{HashMap, HashSet};

impl Block {
    /// Performs a comprehensive validation of a block's contents and rules.
    /// This is a critical function for ensuring the integrity of the blockchain. It's called when adding a new block.
    pub fn validate_block(&self, blockchain: &Blockchain, expected_target: &U256) -> Result<()> {
        info!(
            "DEBUG: Starting validate_block for index {} hash {}",
            self.index,
            self.id().unwrap_or_default()
        );
        info!("DEBUG: expected_target: {}", expected_target);
        let block_hash = self.id()?;
        info!(
            "Validating block with hash: {} at index {}",
            block_hash, self.index
        );

        // 1. Check if the block's hash meets its own declared PoW target.
        if !block_hash.matches_target(self.target) {
            return Err(anyhow!(
                "Block hash ({}) does not meet its own target ({}) (PoW failed)",
                block_hash,
                self.target
            ));
        }
        debug!("PoW valid against block's own target.");

        // 2. Check if the block's declared target matches the one calculated by our DAA.
        if self.target != *expected_target {
            // `expected_target` is calculated by the node before calling this function.
            return Err(anyhow!(
                "Block's declared target ({}) does not match expected target ({}) for index {}",
                self.target,
                expected_target,
                self.index
            ));
        }
        debug!("Block's target matches expected target.");

        // 3. Verify the Merkle root.
        let calculated_merkle_root = MerkleRoot::calculate(&self.transactions)
            .context("Failed to calculate Merkle root during block validation")?;
        if calculated_merkle_root != self.merkle_root {
            return Err(anyhow!(
                "Merkle root mismatch. Expected: {:?}, Calculated: {:?}",
                self.merkle_root,
                calculated_merkle_root
            ));
        }
        debug!("Merkle root validation passed.");

        // 4. Check if the timestamp is not too far in the future.
        let now = Utc::now();
        if self.timestamp > now + ChronoDuration::seconds(crate::MAX_BLOCK_FUTURE_TIMESTAMP as i64)
        {
            return Err(anyhow!("Block timestamp is too far in the future"));
        }
        debug!("Timestamp validation passed.");

        // 5. Check if the timestamp is greater than the median time of the past 11 blocks.
        if self.index > 0 {
            if self.timestamp.timestamp()
                <= Block::calculate_median_time_past(self.index, blockchain)?
            {
                return Err(anyhow!(
                    "Block timestamp is not greater than the median time of past 11 blocks."
                ));
            }
        }

        // 6. Ensure the block is not empty.
        if self.transactions.is_empty() {
            return Err(anyhow!(
                "Block must contain at least a coinbase transaction."
            ));
        }

        // 7. Verify the coinbase transaction.
        info!("DEBUG: About to verify coinbase transaction and calculate total fees.");
        let coinbase_tx = self
            .transactions
            .get(0)
            .ok_or_else(|| anyhow!("Block has no coinbase transaction"))?;
        if !coinbase_tx.is_coinbase() {
            return Err(anyhow!(
                "First transaction in block is not a coinbase transaction."
            ));
        }

        // Correctly calculate total fees by iterating through non-coinbase transactions
        // and using the blockchain's reliable fee calculation method.
        let mut total_fees_in_block = Amount::zero();
        for tx in self.transactions.iter().skip(1) {
            let fee = blockchain.calculate_transaction_fee(tx)?;
            total_fees_in_block = (total_fees_in_block + fee)
                .context("Fee summation overflow during block validation")?;
        }

        self.verify_coinbase_transaction(total_fees_in_block)
            .context("Coinbase transaction verification failed")?;
        info!("DEBUG: Coinbase transaction validation passed.");

        // 8. Verify all other (regular) transactions in the block.
        info!("DEBUG: About to verify regular transactions.");
        self.verify_transactions(self.index, blockchain.utxos())
            .context("Regular transactions verification failed")?;
        info!("DEBUG: Regular transactions validation passed.");

        Ok(())
    }

    /// Verifies all non-coinbase transactions within the block.
    /// This includes checking for double-spends within the block, verifying signatures,
    /// and ensuring that input values are sufficient to cover output values.
    pub fn verify_transactions(
        &self,
        _predicted_block_height: u64,
        chain_utxos: &HashMap<OutPoint, (bool, TransactionOutput)>,
    ) -> Result<()> {
        info!(
            "DEBUG: verify_transactions called for block index {}",
            chain_utxos.len()
        );
        // Keep track of inputs spent within this block to prevent intra-block double spends.
        let mut inputs_in_block: HashSet<OutPoint> = HashSet::new();
        let mut new_outputs_in_block: HashMap<OutPoint, TransactionOutput> = HashMap::new();
        if self.transactions.is_empty() {
            return Err(anyhow!("Empty transactions"));
        }

        for transaction in self.transactions.iter().skip(1) {
            let tx_hash_for_verification = transaction.txid()?;
            info!(
                "DEBUG: Verifying regular transaction {} in block {}",
                tx_hash_for_verification, self.index
            );

            let mut input_value = Amount::zero();
            let mut output_value = Amount::zero();
            let mut inputs_checked_in_tx: HashSet<OutPoint> = HashSet::new();

            if transaction.inputs.is_empty() {
                return Err(anyhow!(
                    "Non-coinbase transaction {} has no inputs",
                    tx_hash_for_verification
                ));
            }
            if transaction.outputs.is_empty() {
                return Err(anyhow!(
                    "Non-coinbase transaction {} has no outputs",
                    tx_hash_for_verification
                ));
            }

            for input in &transaction.inputs {
                let outpoint = &input.outpoint;

                // Check for double spend within the same block.
                if inputs_in_block.contains(outpoint) {
                    return Err(anyhow!("Double spend within block: input {}", outpoint));
                }
                if !inputs_checked_in_tx.insert(*outpoint) {
                    return Err(anyhow!(
                        "Duplicate input within transaction {}: {}",
                        tx_hash_for_verification,
                        outpoint
                    ));
                }

                // Find the output being spent. It could be from a previous block (in `chain_utxos`)
                // or from an earlier transaction in this same block (in `new_outputs_in_block`).
                let prev_output = if let Some(output) = new_outputs_in_block.get(outpoint) {
                    output.clone()
                } else if let Some((_, output)) = chain_utxos.get(outpoint) {
                    output.clone()
                } else {
                    return Err(anyhow!(
                        "Transaction input UTXO {} not found in current UTXO set or within this block",
                        outpoint
                    ));
                };

                // After confirming the UTXO exists, mark it as spent for this block's context.
                // This must be done *after* finding the UTXO but *before* signature verification.
                inputs_in_block.insert(*outpoint);

                // Verify the signature.
                let is_signature_valid = match input.signature.as_ref() {
                    Some(sig) => prev_output
                        .pubkey
                        .0
                        .verify(&tx_hash_for_verification.as_bytes(), sig)
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

                input_value = (input_value + prev_output.value)?;
            }

            for output in &transaction.outputs {
                output_value = (output_value + output.value)?;
            }

            // Add the newly created outputs to a temporary map for subsequent transactions in this block to use.
            for (vout, output) in transaction.outputs.iter().enumerate() {
                new_outputs_in_block.insert(
                    OutPoint {
                        txid: tx_hash_for_verification,
                        vout: vout as u32,
                    },
                    output.clone(),
                );
            }

            // Ensure inputs >= outputs.
            if input_value < output_value {
                return Err(anyhow!(
                    "Insufficient funds in transaction {}: inputs ({}) < outputs ({})",
                    tx_hash_for_verification,
                    input_value,
                    output_value
                ));
            }
        }
        info!(
            "DEBUG: Finished verify_transactions for block index {}",
            self.index
        );

        Ok(())
    }

    /// Verifies the coinbase transaction (the first transaction in a block).
    /// It checks that it has no inputs and that its output value equals the
    /// block reward plus the sum of all transaction fees in the block.
    pub fn verify_coinbase_transaction(&self, total_fees_in_block: Amount) -> Result<()> {
        if self.transactions.is_empty() {
            return Err(anyhow!("Block has no transactions (missing coinbase)"));
        }

        // BIP 34: Coinbase script must start with the block height.
        let coinbase_input = self.transactions[0]
            .inputs
            .get(0)
            .ok_or_else(|| anyhow!("Coinbase transaction has no inputs"))?;
        let coinbase_data = coinbase_input
            .coinbase_data
            .as_ref()
            .ok_or_else(|| anyhow!("Coinbase input is missing coinbase_data (scriptSig)"))?;
        if coinbase_data.len() < 8 {
            return Err(anyhow!(
                "Coinbase data is too short to contain block height"
            ));
        }
        let height_from_coinbase = u64::from_le_bytes(coinbase_data[0..8].try_into()?);

        let coinbase_transaction = self
            .transactions
            .get(0)
            .ok_or_else(|| anyhow!("Block is empty, missing coinbase transaction"))?;

        if coinbase_transaction.inputs.len() != 1 {
            return Err(anyhow!("Coinbase transaction must have exactly one input"));
        }

        if coinbase_transaction.outputs.is_empty() {
            return Err(anyhow!("Coinbase transaction must have outputs"));
        }

        if height_from_coinbase != self.index {
            return Err(anyhow!(
                "Coinbase height mismatch. Block index: {}, Coinbase height: {}",
                self.index,
                height_from_coinbase
            ));
        }

        let block_reward = calculate_block_reward(self.index);
        let expected_total_coinbase = (block_reward + total_fees_in_block)?;
        let mut actual_total_coinbase_outputs = Amount::zero();
        for output in &coinbase_transaction.outputs {
            actual_total_coinbase_outputs = (actual_total_coinbase_outputs + output.value)
                .context("Coinbase transaction output sum overflowed")?;
        }

        if actual_total_coinbase_outputs != expected_total_coinbase {
            return Err(anyhow!(
                "Invalid coinbase reward. Expected: {}, Actual: {}",
                expected_total_coinbase,
                actual_total_coinbase_outputs
            ));
        }
        Ok(())
    }

    /// Calculates the median timestamp of the last 11 blocks.
    fn calculate_median_time_past(block_index: u64, blockchain: &Blockchain) -> Result<i64> {
        let mut timestamps = Vec::with_capacity(11);

        let start_index = block_index.saturating_sub(1);
        let end_index = start_index.saturating_sub(10);

        for i in end_index..=start_index {
            if let Some(block) = blockchain.get_block_by_index(i)? {
                timestamps.push(block.timestamp.timestamp());
                if block.index == 0 {
                    break;
                }
            } else {
                break;
            }
        }

        if timestamps.is_empty() {
            return Ok(0);
        }

        timestamps.sort_unstable();

        Ok(timestamps[timestamps.len() / 2])
    }
}
