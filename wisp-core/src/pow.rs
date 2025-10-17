use crate::{
    blockchain::{Block, Blockchain},
    currency::Amount,
    transactions::{OutPoint, TransactionOutput},
    DAA_WINDOW, IDEAL_BLOCK_TIME, U256,
};

use crate::reorg::ReorgError;
use crate::sha256::Hashable;
use anyhow::{anyhow, Result};
use sha2::Digest;
use sled::transaction::ConflictableTransactionError;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

impl Block {
    /// Calculates the total fees for all non-coinbase transactions in the block.
    /// It does this by summing all inputs and subtracting all outputs.
    /// This requires looking up the value of the inputs from the provided UTXO set.
    pub fn calculate_total_fees(
        &self,
        existing_utxos: &HashMap<OutPoint, (bool, TransactionOutput)>,
    ) -> Result<Amount> {
        let mut inputs_total = Amount::zero();
        let mut outputs_total = Amount::zero();

        // Build a map of new outputs created within this block to handle intra-block spends.
        let mut new_outputs_in_block = HashMap::new();
        for transaction in self.transactions.iter().skip(1) {
            let txid = transaction.txid()?;
            for (vout, output) in transaction.outputs.iter().enumerate() {
                new_outputs_in_block.insert(
                    OutPoint {
                        txid,
                        vout: vout as u32,
                    },
                    output.clone(),
                );
            }
        }

        // Iterate through regular transactions to sum inputs and outputs.
        for transaction in self.transactions.iter().skip(1) {
            // For each input, find its corresponding output (either from a previous block or this one).
            for input in &transaction.inputs {
                // First, check for outputs created in this same block.
                // Then, fall back to the existing UTXO set from the wider chain state.
                let prev_output = new_outputs_in_block.get(&input.outpoint).or_else(|| {
                    existing_utxos
                        .get(&input.outpoint)
                        .map(|(_, output)| output)
                });

                let prev_output = prev_output.ok_or_else(|| {
                    anyhow!(
                        "Transaction input UTXO {} not found for fee calculation",
                        input.outpoint
                    )
                })?;
                let sum_result = inputs_total + prev_output.value;
                inputs_total = sum_result?;
            }

            for output in &transaction.outputs {
                let sum_result = outputs_total + output.value;
                outputs_total = sum_result?;
            }
        }

        if inputs_total < outputs_total {
            Err(anyhow!(
                "Input value less than output value in fee calculation: inputs ({}) < outputs ({})",
                inputs_total,
                outputs_total
            ))
        } else {
            inputs_total - outputs_total
        }
    }

    /// A specialized version of `calculate_total_fees` for use within a database transaction during a reorg.
    /// It fetches UTXO values directly from the transactional database view (`tx_db`) instead of an in-memory map.
    /// This is crucial for maintaining atomicity during the reorg process.
    pub fn calculate_total_fees_for_reorg(
        &self,
        tx_db: &sled::transaction::TransactionalTree,
    ) -> Result<Amount, ConflictableTransactionError<ReorgError>> {
        let mut inputs_total = Amount::zero();
        let mut outputs_total = Amount::zero();

        let mut new_outputs_in_block = HashMap::new();
        for transaction in self.transactions.iter().skip(1) {
            let txid = transaction.txid().map_err(ReorgError::Anyhow)?;
            for (vout, output) in transaction.outputs.iter().enumerate() {
                new_outputs_in_block.insert(
                    OutPoint {
                        txid,
                        vout: vout as u32,
                    },
                    output.clone(),
                );
            }
        }

        for transaction in self.transactions.iter().skip(1) {
            for input in &transaction.inputs {
                let prev_output = if let Some(output) = new_outputs_in_block.get(&input.outpoint) {
                    Some(output.clone())
                } else {
                    Blockchain::find_output_for_reorg_static(tx_db, &input.outpoint, &[])?
                };

                let prev_output = prev_output.ok_or_else(|| {
                    ReorgError::Anyhow(anyhow!(
                        "Transaction input UTXO not found during reorg fee calculation..."
                    ))
                })?;
                inputs_total =
                    (inputs_total + prev_output.value).map_err(|e| ReorgError::Anyhow(e))?;
            }

            for output in &transaction.outputs {
                outputs_total =
                    (outputs_total + output.value).map_err(|e| ReorgError::Anyhow(e))?;
            }
        }

        (inputs_total - outputs_total)
            .map_err(|e| ReorgError::Anyhow(e))
            .map_err(ConflictableTransactionError::Abort)
    }

    /// A simple, single-threaded mining function for testing purposes.
    /// It iterates through nonces until a valid proof-of-work is found.
    pub fn mine_block(&mut self, steps: usize) -> Result<bool> {
        if self.id()?.matches_target(self.target) {
            println!("Block already matches target before mining.");
            return Ok(true);
        }

        for _i in 0..steps {
            self.nonce = self.nonce.wrapping_add(1);

            if self.id()?.matches_target(self.target) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Checks if the block's hash meets the proof-of-work requirement defined by its target.
    pub fn check_proof_of_work(&self) -> bool {
        let hash = self.id().expect("Failed to hash block for PoW check");
        hash.matches_target(self.target)
    }
}

/// A highly optimized, parallelizable mining function.
/// It uses the canonical `Hashable` trait to construct the block hash, ensuring consistency.
/// To maintain performance, it uses a `hasher.clone()` optimization.
/// It hashes all the header fields that come before the nonce once, clones the hasher state,
/// and then only hashes the nonce and remaining fields in the hot loop.
pub fn mine_block_parallel(
    block: &mut Block,
    start_nonce: u64,
    nonce_step: u64,
    max_attempts_per_call: usize,
    mining_active: &AtomicBool,
) -> Result<(bool, usize)> {
    block.nonce = block.nonce.wrapping_add(start_nonce);

    let header = block.header();

    // 1. Create a hasher and hash all parts of the header that come *before* the nonce.
    let mut prefix_hasher = sha2::Sha256::new();
    header.version.update_hasher(&mut prefix_hasher);
    prefix_hasher.update(&header.timestamp.timestamp().to_be_bytes());
    prefix_hasher.update(&header.timestamp.timestamp_subsec_nanos().to_be_bytes());
    header.previous_hash.update_hasher(&mut prefix_hasher);
    header.merkle_root.update_hasher(&mut prefix_hasher);
    header.target.update_hasher(&mut prefix_hasher);

    // 2. The main mining loop.
    for i in 0..max_attempts_per_call {
        if !mining_active.load(Ordering::Relaxed) {
            return Ok((false, i));
        }

        // 3. In the hot loop, clone the prefix hasher and update only with the nonce.
        let mut hasher = prefix_hasher.clone();
        block.nonce.update_hasher(&mut hasher);

        // 4. Perform the double SHA-256 hash and check against the target.
        let first_pass = hasher.finalize();
        let mut hasher2 = sha2::Sha256::new();
        hasher2.update(&first_pass);
        let hash_bytes: [u8; 32] = hasher2.finalize().into();
        let hash_u256 = U256::from_big_endian(&hash_bytes);

        // Debug-only sanity check: print hash and target as big-endian hex to ensure consistent interpretation.
        // This will not run in release builds.
        debug_assert!({
            let target_be = U256::to_big_endian(&block.target);
            log::debug!(
                "mining debug — nonce: {}, hash: {}, target: {}",
                block.nonce,
                hex::encode(hash_bytes),
                hex::encode(target_be)
            );
            true
        });

        if hash_u256 <= block.target {
            return Ok((true, i + 1));
        }

        block.nonce = block.nonce.wrapping_add(nonce_step);
    }
    Ok((false, max_attempts_per_call))
}

impl Blockchain {
    /// Calculates the next proof-of-work target based on the time it took to mine the last `DAA_WINDOW` blocks.
    /// This is the Difficulty Adjustment Algorithm (DAA).
    /// This function is now a wrapper around `calculate_next_target_from_height`.
    pub fn calculate_next_target(&self) -> Result<U256> {
        let current_height = self.block_height()?;
        self.calculate_next_target_from_height(current_height)
    }

    /// Calculates the next proof-of-work target for a block that would be at `height + 1`.
    /// The calculation is based on the window of blocks ending at the specified `height`.
    pub fn calculate_next_target_from_height(&self, height: u64) -> Result<U256> {
        // Special case: genesis block (height 0) or early blocks before full DAA window
        if height == 0 || height < DAA_WINDOW as u64 {
            return Ok(crate::MAX_TARGET);
        }

        // Safe indices for the DAA window
        let last_block_index = height;
        let first_block_index = height.saturating_sub(DAA_WINDOW as u64 - 1);

        let last_block = self.get_block_by_index(last_block_index)?.ok_or_else(|| {
            anyhow::anyhow!(
                "DAA: Missing last block in window at index {}",
                last_block_index
            )
        })?;

        let first_block = self.get_block_by_index(first_block_index)?.ok_or_else(|| {
            anyhow::anyhow!(
                "DAA: Missing first block in window at index {}",
                first_block_index
            )
        })?;

        // Time span of the window
        let mut actual_timespan =
            last_block.timestamp.timestamp() - first_block.timestamp.timestamp();
        actual_timespan = std::cmp::max(1, actual_timespan);

        let ideal_timespan = ((DAA_WINDOW - 1) as u64 * IDEAL_BLOCK_TIME) as i64;
        let clamped_timespan = actual_timespan.clamp(ideal_timespan / 4, ideal_timespan * 4);

        let current_target = last_block.target;

        let avg_time_u256 = U256::from(clamped_timespan as u64);
        let ideal_time_u256 = U256::from(ideal_timespan as u64);

        let numerator = current_target
            .checked_mul(avg_time_u256)
            .ok_or_else(|| anyhow::anyhow!("DAA: Overflow during target multiplication"))?;

        let half_ideal = ideal_time_u256 / U256::from(2u64);
        let numerator = numerator
            .checked_add(half_ideal)
            .ok_or_else(|| anyhow::anyhow!("DAA: Overflow during rounding add"))?;

        let mut new_target = numerator / ideal_time_u256;

        // Clamp to global bounds
        new_target = new_target.min(crate::MAX_TARGET);
        new_target = new_target.max(crate::MIN_TARGET);

        Ok(new_target)
    }

    /// A helper function to determine what the target *should have been* for a given block.
    pub fn calculate_expected_target_for_block(&self, block: &Block) -> Result<U256> {
        if block.index == 0 {
            return Ok(crate::MAX_TARGET);
        }

        self.calculate_next_target_from_height(block.index - 1)
    }
}
