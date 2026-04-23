use crate::{
    blockchain::{Block, Blockchain},
    currency::Amount,
    sha256::Hash,
    storage::DBKeys, transactions::{OutPoint, Script},
};
use anyhow::{anyhow, Result};
use bincode::config::standard as bincode_config;
use log::{info, warn};
use sled::transaction::{ConflictableTransactionError, TransactionalTree};

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
    pub fn reorganize_chain(
        &mut self,
        new_chain_segment: Vec<Block>,
        common_ancestor_index: u64,
    ) -> Result<()> {
        info!(
            "[REORG] Initiating chain reorganization from common ancestor index {}. New segment length: {}",
            common_ancestor_index,
            new_chain_segment.len()
        );
        let mut rolled_back_blocks: Vec<Block> = Vec::new();
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

        if new_chain_segment.is_empty() {
            return Err(anyhow!("Cannot reorganize with an empty new chain segment"));
        }

        self.db
            .transaction(
                |tx_db| -> Result<(), ConflictableTransactionError<ReorgError>> {
                    let mut new_supply = self.total_supply.as_smallest_unit();
                    let mut new_tx_count = self.total_tx_count;
                    for block_to_revert in &rolled_back_blocks {
                        let hash = block_to_revert.id().map_err(ReorgError::Anyhow)?;

                        let block_reward =
                            crate::utils::calculate_block_reward(block_to_revert.index);
                        new_supply = new_supply.saturating_sub(block_reward.as_smallest_unit());

                        tx_db.remove(DBKeys::block(&hash))?;
                        tx_db.remove(DBKeys::index_to_hash(block_to_revert.index))?;
                        tx_db.remove(DBKeys::hash_to_index(&hash))?;
                        tx_db.remove(DBKeys::prev_to_current(&block_to_revert.header.previous_hash))?;

                        for tx in block_to_revert.transactions.iter().rev() {
                            let tx_hash = tx.txid().map_err(ReorgError::Anyhow)?;

                            if !tx.is_coinbase() {
                                new_tx_count = new_tx_count.saturating_sub(1);
                                tx_db.remove(DBKeys::tx_by_order(new_tx_count))?;
                                tx_db.remove(DBKeys::tx_location(&tx_hash))?;

                                for (vout, output) in tx.outputs.iter().enumerate() {
                                    let history_key = match &output.script {
                                        Script::Classic(pk) => Some(DBKeys::history(&pk.fingerprint())),
                                        Script::Shadow(h) | Script::ShadowScript(h) | Script::Aurora(h) | Script::AuroraScript(h) => {
                                            Some(DBKeys::history(&h.to_string()))
                                        }
                                    };
                                    if let Some(key) = history_key {
                                        Self::remove_hash_from_history_list(tx_db, &key, &tx_hash)?;
                                    }

                                    let addr_id = match &output.script {
                                        Script::Classic(pk) => Some(pk.fingerprint()),
                                        Script::Shadow(h) | Script::ShadowScript(h) | Script::Aurora(h) | Script::AuroraScript(h) => Some(h.to_string()),
                                    };
                                    if let Some(id) = addr_id {
                                        crate::storage::remove_outpoint_from_address_utxos(tx_db, &id, &OutPoint { txid: tx_hash, vout: vout as u32 })?;
                                    }
                                }
                            }

                            if !tx.is_coinbase() {
                                for input in &tx.inputs {
                                    let spent_output =
                                        Self::find_output_for_reorg_static(tx_db, &input.outpoint, &[])?;
                                    let output = spent_output.ok_or_else(|| {
                                        ReorgError::Anyhow(anyhow!(
                                            "Reorg failed: Could not find spent output for input {} when reverting block {}",
                                            input.outpoint,
                                            block_to_revert.index
                                        ))
                                    })?;

                                    let fp = match (&output.script, &input.public_key) {
                                        (Script::Classic(pk), _) => Some(pk.fingerprint()),
                                        (_, Some(pk)) => Some(pk.fingerprint()),
                                        _ => None,
                                    };
                                    if let Some(fingerprint) = fp {
                                        let key = DBKeys::history(&fingerprint);
                                        Self::remove_hash_from_history_list(tx_db, &key, &tx_hash)?;
                                    }
                                }
                            }
                        }
                    }


                    let (mut temp_utxos, last_snapshot_height) =
                        if let Some(snapshot_ivec) = tx_db.get(DBKeys::UTXO_SNAPSHOT)? {
                            let decompressed_bytes = zstd::decode_all(&snapshot_ivec[..])
                                .map_err(|e| ReorgError::Anyhow(e.into()))?;
                            let (snapshot, _): (crate::utxo::UtxoSet, _) =
                                bincode::decode_from_slice(&decompressed_bytes, bincode_config())
                                    .map_err(|e| ReorgError::Anyhow(e.into()))?;

                            let height_ivec = tx_db
                                .get(DBKeys::LAST_UTXO_SNAPSHOT_HEIGHT)?
                                .ok_or_else(|| {
                                    ReorgError::Anyhow(anyhow!(
                                        "UTXO snapshot found but snapshot height is missing"
                                    ))
                                })?;
                            let (height, _) =
                                bincode::decode_from_slice(&height_ivec, bincode_config())
                                    .map_err(|e| ReorgError::Anyhow(e.into()))?;
                            
                            if height <= common_ancestor_index {
                                (snapshot, height)
                            } else {
                                warn!("[REORG] UTXO snapshot height {} is ahead of common ancestor {}. Rebuilding from genesis.", height, common_ancestor_index);
                                (crate::utxo::UtxoSet::new(), 0)
                            }
                        } else {
                            (crate::utxo::UtxoSet::new(), 0)
                        };

                    for height in (last_snapshot_height + 1)..=common_ancestor_index {
                        let block = self
                            .get_block_by_index_from_db_txn(height, tx_db)
                            .map_err(ReorgError::Anyhow)?
                            .ok_or_else(|| {
                                ReorgError::Anyhow(anyhow!(
                                    "Failed to find block {} needed to build temp UTXO set for reorg",
                                    height
                                ))
                            })?;
                        temp_utxos.apply_block(&block).map_err(ReorgError::Anyhow)?;
                    }

                    for block_to_apply in &new_chain_segment {
                        let expected_target = self.calculate_next_target_from_height(block_to_apply.index - 1).map_err(ReorgError::Anyhow)?;
                        block_to_apply.validate_block_for_reorg(&temp_utxos.utxos, &expected_target, tx_db).map_err(ReorgError::Anyhow)?;

                        Self::apply_block_to_db(
                            tx_db,
                            block_to_apply,
                            &new_chain_segment,
                            new_supply,
                            new_tx_count,
                        )?;

                        temp_utxos.apply_block(block_to_apply)
                            .map_err(ReorgError::Anyhow)?;
                    }

                    let new_tip_block = new_chain_segment
                        .last()
                        .ok_or_else(|| ReorgError::Anyhow(anyhow!("New chain segment is empty")))?;
                    let new_tip_hash = new_tip_block.id().map_err(ReorgError::Anyhow)?;
                    let new_height = new_tip_block.index;

                    tx_db.insert(DBKeys::CHAIN_HEIGHT, new_height.to_be_bytes().to_vec())?;
                    tx_db.insert(DBKeys::TIP_HASH, bincode::encode_to_vec(&new_tip_hash, bincode_config()).map_err(|e| ReorgError::Anyhow(e.into()))?)?;
                    tx_db.insert(DBKeys::TOTAL_TX_COUNT, new_tx_count.to_be_bytes().to_vec())?;
                    tx_db.insert(DBKeys::TOTAL_SUPPLY, new_supply.to_be_bytes().to_vec())?;

                    let final_utxo_bytes = bincode::encode_to_vec(&temp_utxos, bincode_config())
                        .map_err(|e| ReorgError::Anyhow(e.into()))?;
                    tx_db.insert(DBKeys::PENDING_UTXO_SNAPSHOT, final_utxo_bytes)?;

                    tx_db.remove(DBKeys::PENDING_REORG_TIP)?;
                    tx_db.remove(DBKeys::PENDING_REORG_ANCESTOR)?;
                    tx_db.remove(DBKeys::PENDING_REORG_ANCESTOR_HASH)?;
                    Ok(())
                },
            )
            .map_err(|e| match e {
                sled::transaction::TransactionError::Abort(ReorgError::Anyhow(err)) => err,
                sled::transaction::TransactionError::Storage(err) => anyhow::Error::from(err),
            })?;

        if let Some(bytes) = self.db.get(DBKeys::PENDING_UTXO_SNAPSHOT)? {
            let (snapshot, _): (crate::utxo::UtxoSet, _) =
                bincode::decode_from_slice(&bytes, bincode_config())?;
            self.utxo_set = snapshot.clone();
            self.db
                .transaction(|tx_db| {
                    tx_db.insert(DBKeys::UTXO_SNAPSHOT, bytes.to_vec())?;
                    tx_db.remove(DBKeys::PENDING_UTXO_SNAPSHOT)?;
                    Ok(())
                })
                .map_err(|e: sled::transaction::TransactionError| {
                    anyhow!("Failed to promote UTXO snapshot after reorg: {:?}", e)
                })?;
        } else {
            warn!("[REORG] Pending UTXO snapshot not found after reorg commit. Forcing full UTXO rebuild.");
            self.rebuild_utxos()?;
        }

        let mut txs_to_readd_to_mempool = Vec::new();
        for block_to_revert in rolled_back_blocks.iter().rev() {
            for tx in &block_to_revert.transactions {
                if !tx.is_coinbase() {
                    txs_to_readd_to_mempool.push(tx.clone());
                }
            }
        }

        self.daa_cache
            .retain(|&index, _| index <= common_ancestor_index);
        for block in &new_chain_segment {
            self.daa_cache
                .insert(block.index, (block.header.timestamp, block.header.target));
        }

        for tx in txs_to_readd_to_mempool {
            let tx_hash_for_logging = tx.txid()?;
            if let Err(e) = self.add_transaction_to_mempool(tx, false) {
                warn!(
                    "Could not re-add transaction {} to mempool after reorg (it may be invalid in the new chain context): {}",
                    tx_hash_for_logging, e
                );
            }
        }

        for block in &new_chain_segment {
            self.clear_mempool_of_block_transactions_internal(block, block.id()?, false);
        }

        if let Err(e) = self.save_mempool_snapshot() {
            warn!("[REORG] Failed to save mempool snapshot after reorg: {}", e);
        }

        self.target = self.calculate_next_target()?;
        self.prune_daa_cache(self.block_height()?);
        self.total_supply = Amount::from_smallest_unit(self.get_total_supply_from_db()?);
        self.total_tx_count = self.get_total_transaction_count_from_db()?;
        if let Some(new_tip_block) = new_chain_segment.last() {
            let new_tip_hash = new_tip_block.id()?;
            self.tip_cache = Some((new_tip_hash, new_tip_block.clone()));
        }

        info!(
            "[REORG] Chain reorganization completed successfully. New chain height: {}",
            self.block_height()?
        );
        Ok(())
    }

    pub fn find_output_for_reorg_static(
        tx_db: &sled::transaction::TransactionalTree,
        outpoint: &crate::transactions::OutPoint,
        new_chain_segment: &[Block],
    ) -> Result<
        Option<crate::transactions::TransactionOutput>,
        ConflictableTransactionError<ReorgError>,
    > {
        for block in new_chain_segment {
            for tx in &block.transactions {
                if tx.txid().ok() == Some(outpoint.txid) {
                    if let Some(output) = tx.outputs.get(outpoint.vout as usize) {
                        return Ok(Some(output.clone()));
                    }
                }
            }
        }
        if let Some(ivec) = tx_db.get(DBKeys::tx_location(&outpoint.txid))? {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&ivec);
            let block_index = u64::from_be_bytes(bytes);

            if let Some(hash_ivec) = tx_db.get(DBKeys::index_to_hash(block_index))? {
                let (block_hash, _): (Hash, _) =
                    bincode::decode_from_slice(&hash_ivec, bincode_config()).map_err(|e| {
                        ConflictableTransactionError::Abort(ReorgError::Anyhow(e.into()))
                    })?;

                if let Some(block_ivec) = tx_db.get(DBKeys::block(&block_hash))? {
                    let (checked_block, _): (crate::blockchain::CheckedBlock, _) =
                        bincode::decode_from_slice(&block_ivec, bincode_config()).map_err(|e| {
                            ConflictableTransactionError::Abort(ReorgError::Anyhow(e.into()))
                        })?;
                    let block = checked_block
                        .into_block()
                        .map_err(|e| ConflictableTransactionError::Abort(ReorgError::Anyhow(e)))?;
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

    pub(crate) fn add_hash_to_history_list(
        tx_db: &sled::transaction::TransactionalTree,
        key: &[u8],
        tx_hash: &Hash,
    ) -> Result<(), ConflictableTransactionError<ReorgError>> {
        let mut hashes: Vec<Hash> = tx_db
            .get(key)?
            .and_then(|v| {
                bincode::decode_from_slice::<Vec<Hash>, _>(&v, bincode_config())
                    .ok()
                    .map(|(h, _)| h)
            })
            .unwrap_or_default();

        if hashes.iter().all(|h| h != tx_hash) {
            hashes.push(*tx_hash);
            tx_db.insert(
                key,
                bincode::encode_to_vec(&hashes, bincode_config())
                    .map_err(|e| ReorgError::Anyhow(e.into()))?,
            )?;
        }
        Ok(())
    }

    fn remove_hash_from_history_list(
        tx_db: &sled::transaction::TransactionalTree,
        key: &[u8],
        tx_hash: &Hash,
    ) -> Result<(), ConflictableTransactionError<ReorgError>> {
        if let Some(mut hashes) = tx_db.get(key)?.and_then(|v| {
            bincode::decode_from_slice::<Vec<Hash>, _>(&v, bincode_config())
                .ok()
                .map(|(h, _)| h)
        }) {
            hashes.retain(|h| h != tx_hash);
            if hashes.is_empty() {
                tx_db.remove(key)?;
            } else {
                tx_db.insert(
                    key,
                    bincode::encode_to_vec(&hashes, bincode_config())
                        .map_err(|e| ReorgError::Anyhow(e.into()))?,
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn apply_block_to_db(
        tx_db: &TransactionalTree,
        block_to_apply: &Block,
        new_chain_segment_for_reorg: &[Block],
        initial_supply: u64,
        initial_tx_count: u64,
    ) -> Result<(u64, u64), ConflictableTransactionError<ReorgError>> {
        let block_hash = block_to_apply.id().map_err(ReorgError::Anyhow)?;

        let mut supply = initial_supply;
        let mut tx_count = initial_tx_count;

        let checked_block = crate::blockchain::CheckedBlock::from_block(block_to_apply.clone())
            .map_err(ReorgError::Anyhow)?;
        let checked_block_bytes = bincode::encode_to_vec(&checked_block, bincode_config())
            .map_err(|e| ReorgError::Anyhow(e.into()))?;
        let hash_bytes = bincode::encode_to_vec(&block_hash, bincode_config())
            .map_err(|e| ReorgError::Anyhow(e.into()))?;

        tx_db.insert(DBKeys::block(&block_hash), checked_block_bytes)?;
        tx_db.insert(
            DBKeys::index_to_hash(block_to_apply.index),
            hash_bytes.clone(),
        )?;
        tx_db.insert(
            DBKeys::hash_to_index(&block_hash),
            block_to_apply.index.to_be_bytes().to_vec(),
        )?;
        tx_db.insert(
            DBKeys::prev_to_current(&block_to_apply.header.previous_hash),
            hash_bytes,
        )?;

        let block_reward = crate::utils::calculate_block_reward(block_to_apply.index);
        supply = supply.saturating_add(block_reward.as_smallest_unit());

        tx_db.insert(DBKeys::TOTAL_SUPPLY, supply.to_be_bytes().to_vec())?;

        for tx in &block_to_apply.transactions {
            let tx_hash = tx.txid().map_err(ReorgError::Anyhow)?;
            let tx_hash_bytes = bincode::encode_to_vec(&tx_hash, bincode_config())
                .map_err(|e| ReorgError::Anyhow(e.into()))?;

            for output in &tx.outputs {
                let history_key = match &output.script {
                    Script::Classic(pk) => Some(DBKeys::history(&pk.fingerprint())),
                    Script::Shadow(h) | Script::ShadowScript(h) | Script::Aurora(h) | Script::AuroraScript(h) => {
                        Some(DBKeys::history(&h.to_string()))
                    }
                };
                if let Some(key) = history_key {
                    Self::add_hash_to_history_list(tx_db, &key, &tx_hash)?;
                }
            }

            for (vout, output) in tx.outputs.iter().enumerate() {
                let addr_id = match &output.script {
                    Script::Classic(pk) => Some(pk.fingerprint()),
                    Script::Shadow(h) | Script::ShadowScript(h) | Script::Aurora(h) | Script::AuroraScript(h) => {
                        Some(h.to_string())
                    }
                };
                if let Some(id) = addr_id {
                    crate::storage::add_outpoint_to_address_utxos(tx_db, &id, &OutPoint { txid: tx_hash, vout: vout as u32 })?;
                }
            }

            if !tx.is_coinbase() {
                for input in &tx.inputs {
                    let spent_output = Blockchain::find_output_for_reorg_static(
                        tx_db,
                        &input.outpoint,
                        new_chain_segment_for_reorg,
                    )?
                    .ok_or_else(|| {
                        ReorgError::Anyhow(anyhow!(
                            "UTXO {} not found for history indexing during block apply",
                            input.outpoint
                        ))
                    })?;

                    let addr_id = match &spent_output.script {
                        Script::Classic(pk) => Some(pk.fingerprint()),
                        Script::Shadow(h) | Script::ShadowScript(h) | Script::Aurora(h) | Script::AuroraScript(h) => Some(h.to_string()),
                    };
                    if let Some(id) = addr_id {
                        crate::storage::remove_outpoint_from_address_utxos(tx_db, &id, &input.outpoint)?;
                    }

                    let fp = match (&spent_output.script, &input.public_key) {
                        (Script::Classic(pk), _) => Some(pk.fingerprint()),
                        (_, Some(pk)) => Some(pk.fingerprint()),
                        _ => None,
                    };
                    if let Some(fingerprint) = fp {
                        let key = DBKeys::history(&fingerprint);
                        Self::add_hash_to_history_list(tx_db, &key, &tx_hash)?;
                    }
                }

                tx_db.insert(DBKeys::tx_by_order(tx_count), tx_hash_bytes.clone())?;
                tx_count += 1;
            }

            tx_db.insert(
                DBKeys::tx_location(&tx_hash),
                block_to_apply.index.to_be_bytes().to_vec(),
            )?;
        }
        tx_db.insert(DBKeys::TOTAL_TX_COUNT, tx_count.to_be_bytes().to_vec())?;

        Ok((supply, tx_count))
    }

    pub(crate) fn calculate_block_fees_for_reorg(
        block: &Block,
        tx_db: &TransactionalTree,
    ) -> Result<Amount, ConflictableTransactionError<ReorgError>> {
        let mut total_fees = Amount::zero();

        let mut new_outputs_in_block = std::collections::HashMap::new();
        for tx in block.transactions.iter().skip(1) {
            let txid = tx.txid().map_err(ReorgError::Anyhow)?;
            for (vout, output) in tx.outputs.iter().enumerate() {
                new_outputs_in_block.insert(
                    crate::transactions::OutPoint {
                        txid,
                        vout: vout as u32,
                    },
                    output.clone(),
                );
            }
        }

        for tx in block.transactions.iter().skip(1) {
            let fee = Self::calculate_transaction_fee_for_reorg(tx, tx_db, &new_outputs_in_block)?;
            total_fees = (total_fees.checked_add(fee))
                .ok_or_else(|| ReorgError::Anyhow(anyhow!("Total fee calculation overflow")))?;
        }

        Ok(total_fees)
    }

    pub(crate) fn calculate_transaction_fee_for_reorg(
        transaction: &crate::transactions::Transaction,
        tx_db: &TransactionalTree,
        new_outputs_in_block: &std::collections::HashMap<
            crate::transactions::OutPoint,
            crate::transactions::TransactionOutput,
        >,
    ) -> Result<Amount, ConflictableTransactionError<ReorgError>> {
        if transaction.is_coinbase() {
            return Ok(Amount::zero());
        }

        let mut input_total = Amount::zero();
        for input in &transaction.inputs {
            let prev_output = if let Some(output) = new_outputs_in_block.get(&input.outpoint) {
                Some(output.clone())
            } else {
                Self::find_output_for_reorg_static(tx_db, &input.outpoint, &[])?
            };

            let prev_output = prev_output.ok_or_else(|| {
                ReorgError::Anyhow(anyhow!(
                    "Transaction input UTXO {} not found during reorg fee calculation",
                    input.outpoint
                ))
            })?;

            input_total = (input_total.checked_add(prev_output.value)).ok_or_else(|| {
                ReorgError::Anyhow(anyhow!("Input total overflow in reorg fee calc"))
            })?;
        }

        let output_total: Amount = transaction.outputs.iter().map(|o| o.value).sum();

        input_total
            .checked_sub(output_total)
            .ok_or_else(|| ReorgError::Anyhow(anyhow!("Fee underflow in reorg fee calc")))
            .map_err(ConflictableTransactionError::Abort)
    }
}
