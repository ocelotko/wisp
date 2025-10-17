use crate::{
    blockchain::{Block, Blockchain},
    sha256::Hash,
};
use anyhow::{anyhow, Result};
use log::{info, warn};
use sled::transaction::ConflictableTransactionError;

/// A custom error type for reorganization operations, designed to work with `sled::transaction`.
#[derive(Debug)]
pub enum ReorgError {
    Anyhow(anyhow::Error),
}
impl From<ReorgError> for ConflictableTransactionError<ReorgError> {
    fn from(err: ReorgError) -> Self {
        ConflictableTransactionError::Abort(err)
    }
}

impl Blockchain {
    /// Handles a blockchain reorganization. This is a complex, atomic operation that switches the main chain to a new, longer fork.
    ///
    /// The process involves:
    /// 1. Pre-validating the new chain segment to ensure it's viable.
    /// 2. Atomically updating the database:
    ///    a. Reverting blocks from the old chain back to the common ancestor.
    ///    b. Applying blocks from the new chain segment.
    ///    c. Updating all associated metadata (tip hash, height, supply, transaction indices).
    /// 3. Updating the in-memory state (UTXO set, mempool) to reflect the new chain.
    pub fn reorganize_chain(
        &mut self,
        new_chain_segment: Vec<Block>,
        common_ancestor_index: u64,
    ) -> Result<()> {
        info!(
            "Initiating chain reorganization from common ancestor index {}. New segment length: {}",
            common_ancestor_index,
            new_chain_segment.len()
        );
        let mut rolled_back_blocks: Vec<Block> = Vec::new();
        // 1. Determine which blocks from our current chain need to be reverted.
        let mut current_height = self.block_height()?;

        while current_height > common_ancestor_index {
            if let Some(block_to_revert) = self.get_block_by_index(current_height)? {
                rolled_back_blocks.push(block_to_revert);
                current_height -= 1;
            } else {
                return Err(anyhow!(
                    "Failed to find block at height {} during reorg preparation.",
                    current_height
                ));
            }
        }

        let initial_tx_count = self.get_total_transaction_count_from_db()?;
        let initial_supply = self.get_total_supply_from_db()?;

        // 2. Perform the entire reorg, including validation, inside a single atomic database transaction.
        self.db
            .transaction(
                |tx_db| -> Result<(), ConflictableTransactionError<ReorgError>> {
                    let mut supply = initial_supply;
                    let mut tx_count = initial_tx_count;
                    for block_to_revert in &rolled_back_blocks {
                        // For each reverted block, remove it and its associated data from the DB.
                        let hash = block_to_revert.id().map_err(ReorgError::Anyhow)?;
                        // Calculate fees before adjusting supply.
                        let fees = block_to_revert
                            .calculate_total_fees_for_reorg(tx_db)?
                            .as_smallest_unit();

                        // Adjust total supply by subtracting the coinbase reward and the fees for this reverted block.
                        let block_reward =
                            crate::utils::calculate_block_reward(block_to_revert.index);
                        supply = supply.saturating_sub(block_reward.as_smallest_unit() + fees);

                        tx_db.remove(format!("block_{}", hash).as_bytes())?;
                        tx_db.remove(format!("index_{}", block_to_revert.index).as_bytes())?;

                        for tx in block_to_revert.transactions.iter().rev() {
                            let tx_hash = tx.txid().map_err(ReorgError::Anyhow)?;

                            // For regular (non-coinbase) transactions, revert their metadata.
                            if !tx.is_coinbase() {
                                tx_count = tx_count.saturating_sub(1);
                                let key = format!("tx_by_order_{}", tx_count);
                                tx_db.remove(key.as_bytes())?;
                                tx_db.remove(format!("tx_location_{}", tx_hash).as_bytes())?;

                                // Remove the transaction from each output's public key history index.
                                for output in &tx.outputs {
                                    let key = format!("history_{}", output.pubkey.fingerprint());
                                    self.remove_hash_from_history_list(tx_db, &key, &tx_hash)?;
                                }
                            }

                            for input in &tx.inputs {
                                let spent_output = self.find_output_for_reorg(
                                    tx_db,
                                    &input.outpoint,
                                    &[],
                                )?;
                                let output = spent_output.ok_or_else(|| {
                                    ReorgError::Anyhow(anyhow!(
                                        "Reorg failed: Could not find spent output for input {} when reverting block {}",
                                        input.outpoint,
                                        block_to_revert.index
                                    ))
                                })?;
                                let key = format!("history_{}", output.pubkey.fingerprint());
                                self.remove_hash_from_history_list(tx_db, &key, &tx_hash)?;
                            }
                        }
                    }

                    // Create a temporary UTXO set for validation within the transaction.
                    // Start by reverting the rolled-back blocks from the current in-memory UTXO set.
                    let mut temp_utxos = self.utxo_set.clone();
                    for block_to_revert in &rolled_back_blocks {
                        temp_utxos
                            .revert_block(block_to_revert, |outpoint| {
                                self.find_output_for_reorg(tx_db, outpoint, &[])
                                    .map_err(|conflictable_err| match conflictable_err {
                                        ConflictableTransactionError::Abort(reorg_err) => {
                                            match reorg_err {
                                                ReorgError::Anyhow(err) => err,
                                            }
                                        }
                                        ConflictableTransactionError::Storage(err) => anyhow::Error::from(err),
                                        // This arm is necessary because ConflictableTransactionError is non-exhaustive.
                                        _ => anyhow::anyhow!("An unexpected, non-abort error occurred during UTXO set reversion in reorg."),
                                    })
                            })
                            .map_err(ReorgError::Anyhow)?;
                    }

                    // For each new block, add it and its associated data to the DB.
                    for block_to_apply in &new_chain_segment {
                        // Validate the new block against the temporary state.
                        let expected_target = self
                            .calculate_next_target_from_height(block_to_apply.index - 1)
                            .map_err(ReorgError::Anyhow)?;

                        // We need a temporary Blockchain view for validation.
                        let temp_blockchain_view = Blockchain {
                            utxo_set: temp_utxos.clone(),
                            target: expected_target,
                            db: self.db.clone(), // Not used in validate_block, but needed for struct
                            mempool: self.mempool.clone(), // Not used, but needed
                        };

                        block_to_apply
                            .validate_block(&temp_blockchain_view, &expected_target)
                            .map_err(ReorgError::Anyhow)?;

                        let hash = block_to_apply.id().map_err(ReorgError::Anyhow)?;
                        let block_bytes = bincode::serialize(block_to_apply)
                            .map_err(|e| ReorgError::Anyhow(e.into()))?; // This is fine
                        let hash_bytes =
                            bincode::serialize(&hash).map_err(|e| ReorgError::Anyhow(e.into()))?;

                        tx_db.insert(format!("block_{}", hash).as_bytes(), block_bytes)?;
                        tx_db.insert(
                            format!("index_{}", block_to_apply.index).as_bytes(),
                            hash_bytes,
                        )?;

                        // Apply the block to our temporary UTXO set for the next iteration's validation.
                        temp_utxos.apply_block(block_to_apply)
                            .map_err(ReorgError::Anyhow)?;

                        for tx in &block_to_apply.transactions {
                            let tx_hash = tx.txid().map_err(ReorgError::Anyhow)?;
                            if tx.inputs.is_empty() {
                                // Adjust total supply by adding the new coinbase reward and fees.
                                let block_reward =
                                    crate::utils::calculate_block_reward(block_to_apply.index);
                                let fees = block_to_apply
                                    .calculate_total_fees_for_reorg(tx_db)?
                                    .as_smallest_unit();
                                supply = supply.saturating_add(block_reward.as_smallest_unit() + fees);
                            }

                            let tx_hash_bytes = bincode::serialize(&tx_hash)
                                .map_err(|e| ReorgError::Anyhow(e.into()))?;
                            // Add the transaction to the public key's history index.
                            for output in &tx.outputs {
                                let key = format!("history_{}", output.pubkey.fingerprint());
                                self.add_hash_to_history_list(tx_db, &key, &tx_hash)?;
                            }
                            for input in &tx.inputs {
                                let spent_output = self.find_output_for_reorg(
                                    tx_db,
                                    &input.outpoint,
                                    &new_chain_segment,
                                )?;
                                let output = spent_output.ok_or_else(|| {
                                    ReorgError::Anyhow(anyhow!(
                                        "Reorg failed: Could not find spent output for input {} when applying block {}",
                                        input.outpoint,
                                        block_to_apply.index
                                    ))
                                })?;
                                let key = format!("history_{}", output.pubkey.fingerprint());
                                self.add_hash_to_history_list(tx_db, &key, &tx_hash)?;
                            }

                            if !tx.inputs.is_empty() { // Add transaction metadata.
                                tx_db.insert(
                                    format!("tx_by_order_{}", tx_count).as_bytes(),
                                    tx_hash_bytes.clone(),
                                )?;
                                tx_count += 1;
                            }
                            tx_db.insert(format!("tx_location_{}", tx_hash).as_bytes(), &block_to_apply.index.to_be_bytes())?;
                        }
                    }

                    // Update the final chain state metadata.
                    let new_tip_block = new_chain_segment
                        .last()
                        .ok_or_else(|| ReorgError::Anyhow(anyhow!("New chain segment is empty")))?;
                    let new_tip_hash = new_tip_block.id().map_err(ReorgError::Anyhow)?;
                    let new_height = new_tip_block.index;

                    tx_db.insert(b"chain_height", &new_height.to_be_bytes())?;
                    tx_db.insert(
                        b"tip_hash",
                        bincode::serialize(&new_tip_hash)
                            .map_err(|e| ReorgError::Anyhow(e.into()))?,
                    )?;
                    tx_db.insert(b"total_tx_count", &tx_count.to_be_bytes())?;
                    tx_db.insert(b"total_supply", &supply.to_be_bytes())?;

                    Ok(())
                },
            )
            .map_err(|e| match e {
                sled::transaction::TransactionError::Abort(ReorgError::Anyhow(err)) => err,
                sled::transaction::TransactionError::Storage(err) => anyhow::Error::from(err),
            })?;

        // 3. Update in-memory state.
        let mut txs_to_readd_to_mempool = Vec::new();
        // Collect all transactions from the reverted blocks to potentially re-add to the mempool.
        for block_to_revert in rolled_back_blocks.iter().rev() {
            for tx in &block_to_revert.transactions {
                if !tx.inputs.is_empty() {
                    txs_to_readd_to_mempool.push(tx.clone());
                }
            }
        }
        // Revert the in-memory UTXO set.
        for block_to_revert in rolled_back_blocks.iter().rev() {
            info!(
                "Reverting in-memory state for block: {} (index {})",
                block_to_revert.id().unwrap_or_default(),
                block_to_revert.index
            );
            let db = &self.db;
            self.utxo_set.revert_block(block_to_revert, |outpoint| {
                Self::find_output_by_outpoint_in_db_static(db, outpoint)
            })?;
        }
        // Apply the new blocks to the in-memory UTXO set and clear the mempool of their transactions.
        for block in &new_chain_segment {
            info!(
                "Validating and applying new block from fork: {} (index {})",
                block.id().unwrap_or_default(),
                block.index
            );

            self.utxo_set.apply_block(block)?;
            self.clear_mempool_of_block_transactions(&block);
        }
        // Try to re-add the transactions from the old fork to the mempool. Some may be invalid now.
        for tx in txs_to_readd_to_mempool {
            let tx_hash_for_logging = tx.txid()?;
            if let Err(e) = self.add_to_mempool(tx) {
                warn!(
                    "Could not re-add transaction {} to mempool after reorg (it may be invalid in the new chain context): {}",
                    tx_hash_for_logging,
                    e
                );
            }
        }
        // Recalculate the PoW target for the new chain tip.
        self.target = self.calculate_next_target()?;

        info!(
            "Chain reorganization completed successfully. New chain height: {}",
            self.block_height()?
        );
        Ok(())
    }
    /// A static helper function to find a transaction output during a reorg.
    /// It can look for the output within the new (but not yet committed) chain segment,
    /// or fall back to searching the database via the transactional view.
    pub fn find_output_for_reorg_static(
        tx_db: &sled::transaction::TransactionalTree,
        outpoint: &crate::transactions::OutPoint,
        new_chain_segment: &[Block],
    ) -> Result<
        Option<crate::transactions::TransactionOutput>,
        ConflictableTransactionError<ReorgError>,
    > {
        // First, check if the output was created in one of the new blocks being applied.
        for block in new_chain_segment {
            for tx in &block.transactions {
                if tx.txid().ok() == Some(outpoint.txid) {
                    if let Some(output) = tx.outputs.get(outpoint.vout as usize) {
                        return Ok(Some(output.clone()));
                    }
                }
            }
        }
        // If not, search the database for the transaction that created the output.
        let tx_location_key = format!("tx_location_{}", outpoint.txid);
        if let Some(ivec) = tx_db.get(tx_location_key.as_bytes())? {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&ivec);
            let block_index = u64::from_be_bytes(bytes);

            let block_hash_key = format!("index_{}", block_index);
            if let Some(hash_ivec) = tx_db.get(block_hash_key.as_bytes())? {
                let block_hash: Hash =
                    bincode::deserialize(&hash_ivec).map_err(|e| ReorgError::Anyhow(e.into()))?;
                let block_key = format!("block_{}", block_hash);
                // Optimization: Instead of deserializing the whole block, find the specific transaction
                // and then the output. This avoids unnecessary work if the tx isn't in this block.
                if let Some(block_ivec) = tx_db.get(block_key.as_bytes())? {
                    let block: Block = bincode::deserialize(&block_ivec)
                        .map_err(|e| ReorgError::Anyhow(e.into()))?;
                    return Ok(block
                        .transactions
                        .iter()
                        .find(|t| t.txid().ok() == Some(outpoint.txid))
                        .and_then(|tx| tx.outputs.get(outpoint.vout as usize).cloned()));
                }
            }
        }
        Ok(None)
    }
    /// An instance method wrapper around `find_output_for_reorg_static`.
    fn find_output_for_reorg(
        &self,
        tx_db: &sled::transaction::TransactionalTree,
        outpoint: &crate::transactions::OutPoint,
        new_chain_segment: &[Block],
    ) -> Result<
        Option<crate::transactions::TransactionOutput>,
        ConflictableTransactionError<ReorgError>,
    > {
        Self::find_output_for_reorg_static(tx_db, outpoint, new_chain_segment)
    }
    /// Adds a transaction hash to a list stored under a given key in the database.
    /// This is used to maintain the `history_` index for wallet transaction lookups.
    fn add_hash_to_history_list(
        &self,
        tx_db: &sled::transaction::TransactionalTree,
        key: &str,
        tx_hash: &Hash,
    ) -> Result<(), ConflictableTransactionError<ReorgError>> {
        let mut hashes: Vec<Hash> = tx_db
            .get(key.as_bytes())?
            .and_then(|v| bincode::deserialize(&v).ok())
            .unwrap_or_default();

        if hashes.iter().all(|h| h != tx_hash) {
            hashes.push(*tx_hash);
            tx_db.insert(
                key.as_bytes(),
                bincode::serialize(&hashes).map_err(|e| ReorgError::Anyhow(e.into()))?,
            )?;
        }
        Ok(())
    }
    /// Removes a transaction hash from a list stored under a given key in the database.
    /// This is used to update the `history_` index when reverting blocks.
    fn remove_hash_from_history_list(
        &self,
        tx_db: &sled::transaction::TransactionalTree,
        key: &str,
        tx_hash: &Hash,
    ) -> Result<(), ConflictableTransactionError<ReorgError>> {
        if let Some(mut hashes) = tx_db
            .get(key.as_bytes())?
            .and_then(|v| bincode::deserialize::<Vec<Hash>>(&v).ok())
        {
            hashes.retain(|h| h != tx_hash);
            if hashes.is_empty() {
                tx_db.remove(key.as_bytes())?;
            } else {
                tx_db.insert(
                    key.as_bytes(),
                    bincode::serialize(&hashes).map_err(|e| ReorgError::Anyhow(e.into()))?,
                )?;
            }
        }
        Ok(())
    }
}
