use crate::sha256::Hashable;
use crate::{
    blockchain::{Block, Blockchain},
    DAA_WINDOW, IDEAL_BLOCK_TIME, U256,
};
use anyhow::{anyhow, Result};
use log::warn;
use sha2::Digest;
use std::sync::atomic::{AtomicBool, Ordering};

impl Block {
    /// A simple, single-threaded mining function primarily for testing.
    ///
    /// It iterates through a given number of nonces, attempting to find a hash
    /// that meets the block's target.
    /// Returns `Ok(true)` if a valid nonce is found, `Ok(false)` otherwise.
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
    ///
    /// Returns `true` if `block_hash <= block.target`, `false` otherwise.
    pub fn check_proof_of_work(&self) -> bool {
        let hash = self.id().expect("Failed to hash block for PoW check");
        hash.matches_target(self.target)
    }
}

/// An optimized, parallelizable mining function for a single thread.
///
/// This function is designed to be called by multiple threads in parallel, each with a
/// different `start_nonce` and `nonce_step`.
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
) -> Result<(bool, u64)> {
    block.nonce = block.nonce.wrapping_add(start_nonce);

    // Pre-hash all fields that come before the nonce to optimize the hot loop.
    // This matches the order in `BlockHeader::update_hasher`.
    let mut base_hasher = sha2::Sha256::new();
    block.version.update_hasher(&mut base_hasher);
    base_hasher.update(&block.timestamp.timestamp().to_be_bytes());
    base_hasher.update(&block.timestamp.timestamp_subsec_nanos().to_be_bytes());
    block.previous_hash.update_hasher(&mut base_hasher);
    block.merkle_root.update_hasher(&mut base_hasher);
    block.target.update_hasher(&mut base_hasher);

    for i in 0..max_attempts_per_call {
        if !mining_active.load(Ordering::Relaxed) {
            return Ok((false, i as u64));
        }

        // In the hot loop, clone the pre-hashed state and only hash the nonce.
        let mut hasher = base_hasher.clone();
        block.nonce.update_hasher(&mut hasher);
        let first_pass = hasher.finalize();

        let mut hasher2 = sha2::Sha256::new();
        hasher2.update(&first_pass);
        let hash_bytes: [u8; 32] = hasher2.finalize().into();
        let hash_u256 = U256::from_big_endian(&hash_bytes);

        if hash_u256 <= block.target {
            return Ok((true, (i + 1) as u64));
        }

        block.nonce = block.nonce.wrapping_add(nonce_step);
    }
    Ok((false, max_attempts_per_call as u64))
}

impl Blockchain {
    /// Calculates the next proof-of-work target using the Difficulty Adjustment Algorithm (DAA).
    ///
    /// The calculation is based on the time it took to mine the last `DAA_WINDOW` blocks,
    /// ending at the current chain tip.
    /// This function is now a wrapper around `calculate_next_target_from_height`.
    pub fn calculate_next_target(&self) -> Result<U256> {
        let current_height = self.block_height()?;
        self.calculate_next_target_from_height(current_height)
    }

    /// Calculates the next proof-of-work target for a block that would be at `height + 1`.
    pub fn calculate_next_target_from_height(&self, height: u64) -> Result<U256> {
        // Special case: genesis block (height 0) or early blocks before full DAA window
        let last_block_index = height;
        let window_size = (DAA_WINDOW - 1) as u64;
        let first_block_index = height.saturating_sub(window_size);

        if first_block_index == 0 {
            return Ok(crate::MAX_TARGET);
        }

        // Attempt to get block data from the DAA cache first.
        let (last_block_timestamp, current_target) =
            if let Some((ts, tgt)) = self.daa_cache.get(&last_block_index) {
                (*ts, *tgt)
            } else {
                // Fallback to DB if not in cache.
                let block = self.get_block_by_index(last_block_index)?.ok_or_else(|| {
                    anyhow!(
                        "DAA: Missing last block in window at index {}",
                        last_block_index
                    )
                })?;
                (block.timestamp, block.target)
            };

        let first_block_timestamp = if let Some((ts, _)) = self.daa_cache.get(&first_block_index) {
            *ts
        } else {
            // Fallback to DB if not in cache.
            let block = self.get_block_by_index(first_block_index)?.ok_or_else(|| {
                anyhow!(
                    "DAA: Missing first block in window at index {}",
                    first_block_index
                )
            })?;
            block.timestamp
        };

        // Time span of the window
        let mut actual_timespan =
            last_block_timestamp.timestamp() - first_block_timestamp.timestamp();

        // Clamp timespan to prevent extreme fluctuations.
        // Also, warn if timestamps seem manipulated.
        actual_timespan = std::cmp::max(1, actual_timespan);
        if actual_timespan < 10 && last_block_index > first_block_index {
            warn!("[DAA] Unusually short timespan ({}) for DAA window between blocks {} and {}. Possible timestamp manipulation.", actual_timespan, first_block_index, last_block_index);
        }

        let ideal_timespan = ((DAA_WINDOW - 1) as u64 * IDEAL_BLOCK_TIME) as i64;
        let clamped_timespan = actual_timespan.clamp(ideal_timespan / 4, ideal_timespan * 4);

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

    /// Calculates the expected proof-of-work target for a given block.
    ///
    /// This is a helper function used during block validation to ensure a received block's
    /// target matches what the consensus rules dictate it should be.
    pub fn calculate_expected_target_for_block(&self, block: &Block) -> Result<U256> {
        if block.index == 0 {
            return Ok(crate::MAX_TARGET);
        }

        self.calculate_next_target_from_height(block.index - 1)
    }
}
