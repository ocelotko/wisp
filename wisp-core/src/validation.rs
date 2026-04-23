use crate::{
    blockchain::{Block, Blockchain},
    currency::Amount,
    transactions::Script,
    transactions::{OutPoint, TransactionOutput},
    utils::calculate_block_reward,
    utils::MerkleRoot,
    U256,
};

use anyhow::{anyhow, Context, Result};
use bincode::config::standard as bincode_config;
use chrono::{Duration as ChronoDuration, Utc};
use ecdsa::signature::Verifier;
use log::{debug, info};
use ripemd::Digest;
use sled::transaction::TransactionalTree;
use std::collections::{HashMap, HashSet};

/// Represents the context in which block validation is occurring.
pub enum ValidationState<'a> {
    Live(&'a Blockchain),
    Reorg {
        temp_utxos: &'a HashMap<OutPoint, TransactionOutput>,
        tx_db: &'a TransactionalTree,
    },
}

impl Block {
    pub fn validate_header_and_pow(&self) -> Result<()> {
        let block_hash = self
            .id()
            .context("Failed to get block hash for PoW validation")?;
        if !block_hash.matches_target(self.header.target) {
            return Err(anyhow!(
                "Block hash ({}) does not meet its own target ({}) (PoW failed)",
                block_hash,
                self.header.target
            ));
        }

        let now = Utc::now();
        if self.header.timestamp
            > now + ChronoDuration::seconds(crate::MAX_BLOCK_FUTURE_TIMESTAMP as i64)
        {
            return Err(anyhow!("Block timestamp is too far in the future"));
        }

        Ok(())
    }
    /// Performs a comprehensive, context-aware validation of a block's contents and consensus rules.
    pub fn validate_block(&self, blockchain: &Blockchain, expected_target: &U256) -> Result<()> {
        self.validate_block_with_state(ValidationState::Live(blockchain), expected_target)
    }

    fn validate_block_with_state(
        &self,
        state: ValidationState,
        expected_target: &U256,
    ) -> Result<()> {
        info!(
            "DEBUG: Starting validate_block for index {} hash {}",
            self.index,
            self.id().unwrap_or_default()
        );

        let encoded_block = bincode::encode_to_vec(self, bincode_config())?;
        if encoded_block.len() > crate::MAX_BLOCK_SIZE_BYTES {
            return Err(anyhow!(
                "Block size ({} bytes) exceeds maximum limit of {} bytes",
                encoded_block.len(),
                crate::MAX_BLOCK_SIZE_BYTES
            ));
        }
        let block_hash = self
            .id()
            .context("Failed to get block hash for validation")?;

        // 1. Validate Proof of Work
        if !block_hash.matches_target(self.header.target) {
            return Err(anyhow!(
                "Block hash ({}) does not meet its own target ({}) (PoW failed)",
                block_hash,
                self.header.target
            ));
        }
        debug!("PoW valid against block's own target.");

        // 2. Validate Target Difficulty
        if self.header.target != *expected_target {
            return Err(anyhow!(
                "Block's declared target ({}) does not match expected target ({}) for index {}",
                self.header.target,
                expected_target,
                self.index
            ));
        }
        debug!("Block's target matches expected target.");

        // 3. Validate Merkle Root
        let calculated_merkle_root = MerkleRoot::calculate(&self.transactions)
            .context("Failed to calculate Merkle root during block validation")?;
        if calculated_merkle_root != self.header.merkle_root {
            return Err(anyhow!(
                "Merkle root mismatch. Expected: {:?}, Calculated: {:?}",
                self.header.merkle_root,
                calculated_merkle_root
            ));
        }
        debug!("Merkle root validation passed.");

        // 4. Validate Timestamp (Future Limit)
        let now = Utc::now();
        if self.header.timestamp
            > now + ChronoDuration::seconds(crate::MAX_BLOCK_FUTURE_TIMESTAMP as i64)
        {
            return Err(anyhow!("Block timestamp is too far in the future"));
        }
        debug!("Timestamp validation passed.");

        // 5. Validate Timestamp (Median Time Past)
        if self.index > 0 {
            let mtp = match state {
                ValidationState::Live(blockchain) => {
                    Block::calculate_median_time_past(self.index, Some(blockchain), None)?
                }
                ValidationState::Reorg { tx_db, .. } => {
                    Block::calculate_median_time_past(self.index, None, Some(tx_db))?
                }
            };

            if self.header.timestamp.timestamp() <= mtp {
                return Err(anyhow!(
                    "Block timestamp ({}) is not greater than the median time of past 11 blocks ({}).",
                    self.header.timestamp.timestamp(), mtp
                ));
            }
        }

        if self.transactions.is_empty() {
            return Err(anyhow!(
                "Block must contain at least a coinbase transaction."
            ));
        }

        // 6. Validate Coinbase Transaction
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

        let total_fees_in_block = match state {
            ValidationState::Live(blockchain) => {
                let mut fees = Amount::zero();
                let mut outputs_created_in_block = HashMap::new();
                for tx in self.transactions.iter().skip(1) {
                    let fee = blockchain
                        .calculate_transaction_fee_with_overlay(tx, &outputs_created_in_block)?;
                    fees = fees
                        .checked_add(fee)
                        .context("Fee sum overflow during block validation")?;

                    let txid = tx.txid()?;
                    for (i, output) in tx.outputs.iter().enumerate() {
                        outputs_created_in_block.insert(
                            OutPoint {
                                txid,
                                vout: i as u32,
                            },
                            output.clone(),
                        );
                    }
                }
                fees
            }
            ValidationState::Reorg { tx_db, .. } => {
                Blockchain::calculate_block_fees_for_reorg(self, tx_db)
                    .map_err(|e| anyhow::anyhow!("Reorg fee calculation failed: {:?}", e))?
            }
        };
        self.verify_coinbase_transaction(total_fees_in_block)
            .context("Coinbase transaction verification failed")?;
        info!("DEBUG: Coinbase transaction validation passed.");

        // 7. Validate Regular Transactions
        let utxos_for_validation = match state {
            ValidationState::Live(blockchain) => blockchain.utxos(),
            ValidationState::Reorg { temp_utxos, .. } => temp_utxos,
        };
        info!("DEBUG: About to verify regular transactions.");
        self.verify_transactions(utxos_for_validation)
            .context("Regular transactions verification failed")?;
        info!("DEBUG: Regular transactions validation passed.");
        Ok(())
    }

    /// A specialized version of `validate_block` for use within a database transaction during a reorg.
    pub fn validate_block_for_reorg(
        &self,
        temp_utxos: &HashMap<OutPoint, TransactionOutput>,
        expected_target: &U256,
        tx_db: &TransactionalTree,
    ) -> Result<()> {
        self.validate_block_with_state(
            ValidationState::Reorg { temp_utxos, tx_db },
            expected_target,
        )
    }

    pub fn verify_transactions(
        &self,
        chain_utxos: &HashMap<OutPoint, TransactionOutput>,
    ) -> Result<()> {
        if self.transactions.len() > crate::MAX_BLOCK_TRANSACTIONS {
            return Err(anyhow!(
                "Block contains too many transactions: {}, max is {}",
                self.transactions.len(),
                crate::MAX_BLOCK_TRANSACTIONS
            ));
        }

        let mut inputs_in_block: HashSet<OutPoint> = HashSet::new();
        let mut new_outputs_in_block: HashMap<OutPoint, TransactionOutput> = HashMap::new();
        let mut tx_hashes_in_block: HashSet<crate::sha256::Hash> = HashSet::new();

        if self.transactions.is_empty() {
            return Err(anyhow!("Empty transactions"));
        }

        for transaction in self.transactions.iter().skip(1) {
            let txid = transaction.txid()?;

            info!(
                "DEBUG: Verifying regular transaction {} in block {}",
                txid, self.index
            );

            let mut input_value = Amount::zero();
            let mut output_value = Amount::zero();
            let mut inputs_checked_in_tx: HashSet<OutPoint> = HashSet::new();

            if !tx_hashes_in_block.insert(txid) {
                return Err(anyhow!("Duplicate transaction {} found in block", txid));
            }

            let encoded_tx = bincode::encode_to_vec(transaction, bincode_config())?;
            if encoded_tx.len() > crate::MAX_TRANSACTION_SIZE_BYTES {
                return Err(anyhow!(
                    "Transaction {} size ({} bytes) exceeds limit of {} bytes",
                    txid,
                    encoded_tx.len(),
                    crate::MAX_TRANSACTION_SIZE_BYTES
                ));
            }

            if transaction.inputs.is_empty() {
                return Err(anyhow!("Non-coinbase transaction {} has no inputs", txid));
            }
            if transaction.outputs.is_empty() {
                return Err(anyhow!("Non-coinbase transaction {} has no outputs", txid));
            }

            for input in &transaction.inputs {
                let outpoint = &input.outpoint;

                if inputs_in_block.contains(outpoint) {
                    return Err(anyhow!("Double spend within block: input {}", outpoint));
                }
                if !inputs_checked_in_tx.insert(*outpoint) {
                    return Err(anyhow!(
                        "Duplicate input within transaction {}: {}",
                        txid,
                        outpoint
                    ));
                }

                let prev_output = if let Some(output) = new_outputs_in_block.get(outpoint) {
                    output.clone()
                } else if let Some(output) = chain_utxos.get(outpoint) {
                    output.clone()
                } else {
                    return Err(anyhow!(
                        "Transaction input UTXO {} not found in current UTXO set or within this block",
                        outpoint
                    ));
                };

                inputs_in_block.insert(*outpoint);

                let is_spend_valid = match &prev_output.script {
                    Script::Classic(pk) => match &input.signature {
                        Some(sig) => pk.0.verify(&txid.as_bytes(), &sig.0).is_ok(),
                        None => false,
                    },
                    Script::Shadow(hash) | Script::Aurora(hash) => {
                        match (&input.signature, &input.public_key) {
                            (Some(sig), Some(pk)) => {
                                let hashed_pk = crate::address::Address::hash160(pk);
                                if hashed_pk != hash.as_bytes()[..20] {
                                    false
                                } else {
                                    pk.0.verify(&txid.as_bytes(), &sig.0).is_ok()
                                }
                            }
                            _ => false,
                        }
                    }
                    Script::ShadowScript(hash) | Script::AuroraScript(hash) => {
                        match &input.redeem_script {
                            Some(rs) => {
                                let hashed_rs = crate::sha256::hash(rs);
                                // ShadowScript uses 20-byte RIPEMD, AuroraScript uses 32-byte SHA
                                let hash_matches =
                                    if matches!(prev_output.script, Script::ShadowScript(_)) {
                                        let mut ripemd = ripemd::Ripemd160::new();
                                        ripemd.update(hashed_rs.as_bytes());
                                        ripemd.finalize().to_vec() == hash.as_bytes()[..20]
                                    } else {
                                        hashed_rs == *hash
                                    };

                                if !hash_matches {
                                    false
                                } else {
                                    // In absence of VM, verify sig against public_key if provided
                                    match (&input.signature, &input.public_key) {
                                        (Some(sig), Some(pk)) => {
                                            pk.0.verify(&txid.as_bytes(), &sig.0).is_ok()
                                        }
                                        (None, _) => true, // Data-only lock
                                        _ => false,
                                    }
                                }
                            }
                            None => false,
                        }
                    }
                };

                if !is_spend_valid {
                    let sig_status = if input.signature.is_none() {
                        "missing"
                    } else {
                        "invalid"
                    };
                    return Err(anyhow!(
                        "Transaction signature {} for input {} in transaction {}",
                        sig_status,
                        outpoint,
                        txid
                    ));
                }

                input_value = input_value
                    .checked_add(prev_output.value)
                    .context("Input value overflow in block validation")?;
            }

            for output in &transaction.outputs {
                if output.value.as_smallest_unit() < crate::MIN_OUTPUT_VALUE {
                    return Err(anyhow!(
                        "Transaction output value {} is below the dust limit of {}",
                        output.value,
                        crate::MIN_OUTPUT_VALUE
                    ));
                }
                output_value = output_value
                    .checked_add(output.value)
                    .context("Output value overflow in block validation")?;
            }

            for (vout, output) in transaction.outputs.iter().enumerate() {
                new_outputs_in_block.insert(
                    OutPoint {
                        txid,
                        vout: vout as u32,
                    },
                    output.clone(),
                );
            }

            if input_value < output_value {
                return Err(anyhow!(
                    "Insufficient funds in transaction {}: inputs ({}) < outputs ({})",
                    txid,
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

    pub fn verify_coinbase_transaction(&self, total_fees_in_block: Amount) -> Result<()> {
        if self.transactions.is_empty() {
            return Err(anyhow!("Block has no transactions (missing coinbase)"));
        }

        let coinbase_input = self.transactions[0]
            .inputs
            .get(0)
            .ok_or_else(|| anyhow!("Coinbase transaction has no inputs"))?;
        let coinbase_data = coinbase_input
            .coinbase_data
            .as_ref()
            .ok_or_else(|| anyhow!("Coinbase input is missing coinbase_data (scriptSig)"))?;

        const MIN_COINBASE_DATA_SIZE: usize = std::mem::size_of::<u64>();
        if coinbase_data.len() < MIN_COINBASE_DATA_SIZE
            || coinbase_data.len() > crate::MAX_COINBASE_DATA_SIZE
        {
            return Err(anyhow!(
                "Coinbase data size ({}) is outside the allowed range of {}-{} bytes",
                coinbase_data.len(),
                MIN_COINBASE_DATA_SIZE,
                crate::MAX_COINBASE_DATA_SIZE
            ));
        }
        let height_bytes = coinbase_data
            .get(0..std::mem::size_of::<u64>())
            .ok_or_else(|| anyhow!("Coinbase data is too short to contain block height"))?;
        let height_from_coinbase = u64::from_le_bytes(height_bytes.try_into()?);

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
        let expected_total_coinbase = block_reward
            .checked_add(total_fees_in_block) // This was already correct
            .context("Expected coinbase value overflowed")?;
        let mut actual_total_coinbase_outputs = Amount::zero();
        for output in &coinbase_transaction.outputs {
            actual_total_coinbase_outputs = actual_total_coinbase_outputs
                .checked_add(output.value)
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

    fn calculate_median_time_past(
        block_index: u64,
        blockchain: Option<&Blockchain>,
        tx_db: Option<&TransactionalTree>,
    ) -> Result<i64> {
        let mut timestamps = Vec::with_capacity(11);

        let start_index = block_index.saturating_sub(1);
        let end_index = start_index.saturating_sub(10);

        for i in end_index..=start_index {
            let block_opt = match (blockchain, tx_db) {
                (Some(bc), None) => bc.get_block_by_index(i)?,
                (None, Some(db)) => Blockchain::get_block_by_index_from_db_txn_static(i, db)?,
                _ => {
                    return Err(anyhow!(
                        "Invalid state for MTP calculation: must provide either blockchain or tx_db"
                    ));
                }
            };
            if let Some(block) = block_opt {
                timestamps.push(block.header.timestamp.timestamp());
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
