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
    pub fn check_proof_of_work(&self) -> bool {
        let hash = self.id().expect("Failed to hash block for PoW check");
        hash.matches_target(self.target)
    }
}

pub fn mine_block_parallel(
    block: &mut Block,
    start_nonce: u64,
    nonce_step: u64,
    max_attempts_per_call: usize,
    mining_active: &AtomicBool,
) -> Result<(bool, u64)> {
    block.nonce = block.nonce.wrapping_add(start_nonce);

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
    pub fn calculate_next_target(&self) -> Result<U256> {
        let current_height = self.block_height()?;
        self.calculate_next_target_from_height(current_height)
    }

    pub fn calculate_next_target_from_height(&self, height: u64) -> Result<U256> {
        let last_block_index = height;
        let window_size = (DAA_WINDOW - 1) as u64;
        let first_block_index = height.saturating_sub(window_size);

        // If we haven't processed enough blocks for a full window, return max target (min difficulty)
        if first_block_index == 0 {
            return Ok(crate::MAX_TARGET);
        }

        // Retrieve timestamps and targets for the window boundaries
        let (last_block_timestamp, current_target) =
            if let Some((ts, tgt)) = self.daa_cache.get(&last_block_index) {
                (*ts, *tgt)
            } else {
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
            let block = self.get_block_by_index(first_block_index)?.ok_or_else(|| {
                anyhow!(
                    "DAA: Missing first block in window at index {}",
                    first_block_index
                )
            })?;
            block.timestamp
        };

        // Calculate actual timespan
        let mut actual_timespan =
            last_block_timestamp.timestamp() - first_block_timestamp.timestamp();

        // Prevent negative or zero timespan
        actual_timespan = std::cmp::max(1, actual_timespan);
        if actual_timespan < 10 && last_block_index > first_block_index {
            warn!("[DAA] Unusually short timespan ({}) for DAA window between blocks {} and {}. Possible timestamp manipulation.", actual_timespan, first_block_index, last_block_index);
        }

        // Dampening: limit adjustment factor to 4x or 0.25x
        let ideal_timespan = ((DAA_WINDOW - 1) as u64 * IDEAL_BLOCK_TIME) as i64;
        let clamped_timespan = actual_timespan.clamp(ideal_timespan / 4, ideal_timespan * 4);

        // Calculate new target: new_target = old_target * (actual_time / ideal_time)
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

        new_target = new_target.min(crate::MAX_TARGET);
        new_target = new_target.max(crate::MIN_TARGET);

        Ok(new_target)
    }

    pub fn calculate_expected_target_for_block(&self, block: &Block) -> Result<U256> {
        if block.index == 0 {
            return Ok(crate::MAX_TARGET);
        }

        self.calculate_next_target_from_height(block.index - 1)
    }
}
