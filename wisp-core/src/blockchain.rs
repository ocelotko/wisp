use crate::{
    currency::Amount,
    mempool::MempoolEntry,
    reorg::ReorgError,
    sha256::{hash, Hash, Hashable, Sha256},
    signatures::PublicKey,
    storage::DBKeys,
    transactions::{OutPoint, Transaction, TransactionInput, TransactionOutput},
    utils::MerkleRoot,
    utxo::UtxoSet,
    U256,
};

use anyhow::Result;
use anyhow::{anyhow, Context};
use bincode::config::standard as bincode_config;
use bincode::{Decode, Encode};
use chrono::{DateTime, Utc};
use log::{error, info, warn};
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sled::transaction::ConflictableTransactionError;
use sled::Db;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;

#[derive(Debug)]
pub enum AddBlockResult {
    Added,
    Rejected(String),
    PotentialLongerForkDetected {
        common_ancestor_index: u64,
        new_block_index: u64,
        new_block_hash: Hash,
    },
    Orphaned,
    OrphanRejected(String),
    ShorterForkRejected(String),
}

#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub header: BlockHeader,
    pub index: u64,
    pub transactions: Vec<Transaction>,
}

#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct CheckedBlock {
    block: Block,
    checksum: Hash,
}

impl CheckedBlock {
    pub(crate) fn from_block(block: Block) -> Result<Self> {
        let block_bytes = bincode::encode_to_vec(&block, bincode_config())?;
        let checksum = hash(&block_bytes[..]);
        Ok(Self { block, checksum })
    }

    pub(crate) fn into_block(self) -> Result<Block> {
        let block_bytes = bincode::encode_to_vec(&self.block, bincode_config())?;
        let expected_checksum = hash(&block_bytes[..]);
        if self.checksum == expected_checksum {
            Ok(self.block)
        } else {
            Err(anyhow!(
                "Block checksum mismatch! Expected {}, got {}. Block data may be corrupt.",
                expected_checksum,
                self.checksum
            ))
        }
    }
}

#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BlockHeader {
    pub version: u32,
    #[bincode(with_serde)]
    pub timestamp: DateTime<Utc>,
    pub nonce: u64,
    pub previous_hash: Hash,
    pub merkle_root: MerkleRoot,
    pub target: U256,
}

impl Hashable for BlockHeader {
    fn update_hasher(&self, hasher: &mut Sha256) {
        self.version.update_hasher(hasher);
        hasher.update(&self.timestamp.timestamp().to_be_bytes());
        hasher.update(&self.timestamp.timestamp_subsec_nanos().to_be_bytes());
        self.previous_hash.update_hasher(hasher);
        self.merkle_root.update_hasher(hasher);
        self.target.update_hasher(hasher);
        self.nonce.update_hasher(hasher);
    }
}

impl Block {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        version: u32,
        timestamp: DateTime<Utc>,
        nonce: u64,
        previous_hash: Hash,
        merkle_root: MerkleRoot,
        target: U256,
        index: u64,
        transactions: Vec<Transaction>,
    ) -> Self {
        Block {
            header: BlockHeader {
                version,
                timestamp,
                nonce,
                previous_hash,
                merkle_root,
                target,
            },
            index,
            transactions,
        }
    }

    pub fn header(&self) -> BlockHeader {
        self.header.clone()
    }

    pub fn id(&self) -> Result<Hash, anyhow::Error> {
        Ok(hash(&self.header))
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Blockchain {
    pub utxo_set: UtxoSet,
    pub target: U256,
    #[serde(skip)]
    pub db: Db,
    #[serde(default)]
    pub mempool: HashMap<Hash, MempoolEntry>,
    #[serde(skip)]
    pub daa_cache: HashMap<u64, (DateTime<Utc>, U256)>,
    #[serde(skip)]
    pub tip_cache: Option<(Hash, Block)>,
    #[serde(skip)]
    pub total_supply: Amount,
    #[serde(skip)]
    pub total_tx_count: u64,
    #[serde(skip)]
    pub orphan_pool: HashMap<Hash, Vec<(DateTime<Utc>, Hash, Block)>>,
    #[serde(skip)]
    pub orphan_cache_by_hash: HashMap<Hash, ()>,
    #[serde(skip)]
    pub orphan_order: VecDeque<(DateTime<Utc>, Hash, Hash)>,
    #[serde(skip)]
    pub mempool_spent_utxos: HashSet<OutPoint>,
}

impl Blockchain {
    pub const UTXO_SNAPSHOT_INTERVAL: u64 = 720;

    pub fn new(db: Db) -> Self {
        Blockchain {
            utxo_set: UtxoSet::new(),
            target: crate::MAX_TARGET,
            db,
            mempool: HashMap::new(),
            daa_cache: HashMap::new(),
            tip_cache: None,
            total_supply: Amount::zero(),
            total_tx_count: 0,
            orphan_pool: HashMap::new(),
            orphan_cache_by_hash: HashMap::new(),
            orphan_order: VecDeque::new(),
            mempool_spent_utxos: HashSet::new(),
        }
    }

    pub fn utxos(&self) -> &HashMap<OutPoint, TransactionOutput> {
        &self.utxo_set.utxos
    }

    pub fn mempool(&self) -> &HashMap<Hash, MempoolEntry> {
        &self.mempool
    }

    pub fn get_mempool_entries_sorted(&self) -> Vec<MempoolEntry> {
        let mut entries: Vec<MempoolEntry> = self.mempool.values().cloned().collect();
        entries.sort_by(|a, b| {
            b.fee
                .cmp(&a.fee)
                .then_with(|| a.timestamp.cmp(&b.timestamp))
        });
        entries
    }

    pub fn get_block_template_for_pubkey(&self, reward_pubkey: &PublicKey) -> Result<Block> {
        self.get_block_template(reward_pubkey, None)
    }

    pub fn get_block_template(
        &self,
        reward_pubkey: &PublicKey,
        coinbase_message: Option<&str>,
    ) -> Result<Block> {
        let previous_hash = self.get_tip_hash()?.unwrap_or_else(Hash::zero);
        let index = self.block_height()? + 1;
        let target = self.calculate_next_target()?;

        let all_mempool_entries = self.get_mempool_entries_sorted();
        let mut transactions_for_block = Vec::new();
        let mut total_fees = Amount::zero();
        let mut estimated_block_size = 0;
        let mut outputs_created_in_block: HashMap<OutPoint, TransactionOutput> = HashMap::new();

        let block_header_and_metadata_estimate = 256;
        estimated_block_size += block_header_and_metadata_estimate;

        for entry in all_mempool_entries {
            let tx = entry.transaction;
            if transactions_for_block.len() + 1 >= crate::MAX_BLOCK_TRANSACTIONS {
                break;
            }

            let tx_size = entry.serialized_size;
            if estimated_block_size + tx_size > crate::MAX_BLOCK_SIZE_BYTES {
                break;
            }

            let fee =
                match self.calculate_transaction_fee_with_overlay(&tx, &outputs_created_in_block) {
                    Ok(f) => f,
                    Err(e) => {
                        log::debug!(
                            "Skipping tx {} in block template: {}",
                            tx.txid().unwrap_or_default(),
                            e
                        );
                        continue;
                    }
                };

            total_fees = total_fees.checked_add(fee).context("Fee sum overflow")?;
            estimated_block_size += tx_size;

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
            transactions_for_block.push(tx);
        }

        let block_reward = crate::utils::calculate_block_reward(index);
        let coinbase_value = block_reward
            .checked_add(total_fees)
            .context("Coinbase value overflow")?;

        let mut coinbase_data = index.to_le_bytes().to_vec();
        if let Some(msg) = coinbase_message {
            let msg_bytes = msg.as_bytes();
            if coinbase_data.len() + msg_bytes.len() <= crate::MAX_COINBASE_DATA_SIZE {
                coinbase_data.extend_from_slice(msg_bytes);
            }
        }

        let coinbase_tx = Transaction::new(
            vec![TransactionInput {
                outpoint: OutPoint {
                    txid: Hash::zero(),
                    vout: u32::MAX,
                },
                signature: None,
                coinbase_data: Some(coinbase_data),
            }],
            vec![TransactionOutput {
                value: coinbase_value,
                pubkey: *reward_pubkey,
            }],
        );

        transactions_for_block.insert(0, coinbase_tx);

        let merkle_root = crate::utils::MerkleRoot::calculate(&transactions_for_block)?;
        let template = Block::new(
            crate::BLOCK_VERSION,
            Utc::now(),
            0,
            previous_hash,
            merkle_root,
            target,
            index,
            transactions_for_block,
        );
        Ok(template)
    }

    pub fn get_target(&self) -> U256 {
        self.target
    }

    pub fn calculate_transaction_fee(&self, transaction: &Transaction) -> Result<Amount> {
        self.calculate_transaction_fee_with_overlay(transaction, &HashMap::new())
    }

    pub fn calculate_transaction_fee_with_overlay(
        &self,
        transaction: &Transaction,
        overlay_utxos: &HashMap<OutPoint, TransactionOutput>,
    ) -> Result<Amount> {
        if transaction.is_coinbase() {
            return Ok(Amount::zero());
        }

        let outpoints_to_find: Vec<OutPoint> =
            transaction.inputs.iter().map(|i| i.outpoint).collect();

        let mut found_outputs = HashMap::new();
        let mut missing_outpoints = Vec::new();

        for outpoint in &outpoints_to_find {
            if let Some(output) = overlay_utxos.get(outpoint) {
                found_outputs.insert(*outpoint, output.clone());
            } else {
                missing_outpoints.push(*outpoint);
            }
        }

        if !missing_outpoints.is_empty() {
            let chain_outputs = self.find_outputs_by_outpoints(&missing_outpoints)?;
            found_outputs.extend(chain_outputs);
        }

        let mut input_total = Amount::zero();
        for input in &transaction.inputs {
            if let Some(prev_output) = found_outputs.get(&input.outpoint) {
                input_total = input_total
                    .checked_add(prev_output.value)
                    .context("Overflow calculating total input value")?;
            } else {
                return Err(anyhow!(
                    "Input UTXO {} not found for fee calculation in transaction {}",
                    input.outpoint,
                    transaction.txid().unwrap_or_default()
                ));
            }
        }

        let output_total: Amount = transaction.outputs.iter().map(|o| o.value).sum();

        input_total
            .checked_sub(output_total)
            .context("Underflow calculating transaction fee (outputs > inputs)")
    }

    pub fn add_block(&mut self, new_block: Block) -> Result<AddBlockResult> {
        let new_block_hash = match new_block.id() {
            Ok(h) => h,
            Err(e) => {
                error!("Failed to hash new block: {}", e);
                return Ok(AddBlockResult::Rejected(format!(
                    "Failed to hash new block: {}",
                    e
                )));
            }
        };
        info!(
            "[CHAIN] Attempting to add block with hash: {} at index {}",
            new_block_hash, new_block.index
        );

        let current_tip_option = self.get_tip_block()?;
        let current_chain_tip = if let Some(block) = current_tip_option {
            block
        } else {
            if new_block.index == 0 && new_block.header.previous_hash == Hash::zero() {
                info!("[CHAIN] Chain is empty, processing genesis block.");
                return self.add_direct_extension(new_block, new_block_hash);
            } else {
                return Ok(AddBlockResult::OrphanRejected(
                    "Chain is not initialized. Cannot accept non-genesis peer blocks.".to_string(),
                ));
            }
        };
        let current_chain_tip_hash = current_chain_tip.id()?;

        if new_block.header.previous_hash == current_chain_tip_hash {
            info!(
                "[CHAIN] New block {} is a direct extension of the current tip.",
                new_block_hash
            );
            return self.add_direct_extension(new_block, new_block_hash);
        } else {
            warn!(
                "[FORK] Fork detected or out-of-order block ({}). Local tip: {} (index {}), New block previous: {} (index {})",
                new_block_hash,
                current_chain_tip_hash, current_chain_tip.index,
                new_block.header.previous_hash, new_block.index.saturating_sub(1)
            );

            if let Err(e) = new_block.validate_header_and_pow() {
                return Ok(AddBlockResult::Rejected(format!(
                    "Invalid block header/PoW: {}",
                    e
                )));
            }

            let checked_block = CheckedBlock::from_block(new_block.clone())?;
            let checked_block_bytes = bincode::encode_to_vec(&checked_block, bincode_config())?;
            self.db
                .insert(DBKeys::block(&new_block_hash), checked_block_bytes)?;

            if let Some((common_ancestor_index, common_ancestor_hash)) =
                self.find_common_ancestor(&new_block.header.previous_hash)
            {
                info!(
                    "[FORK] Found common ancestor {} at index {} for received block {}",
                    common_ancestor_hash, common_ancestor_index, new_block_hash
                );

                if new_block.index <= current_chain_tip.index {
                    info!(
                        "[FORK] Stored block {} (index {}) as part of a shorter or equal fork.",
                        new_block_hash, new_block.index
                    );
                    return Ok(AddBlockResult::ShorterForkRejected(format!(
                    "Received block {} is part of a shorter or equal length fork (new index {} <= current index {}). Stored but not active.",
                        new_block_hash, new_block.index, current_chain_tip.index,
                    )));
                }

                info!(
                    "[REORG] Longer fork detected at block {}. Initiating reorg.",
                    new_block_hash
                );

                let mut new_chain_segment = Vec::new();
                let mut current_backtrack = new_block.clone();
                let mut depth = 0;

                const MAX_REORG_DEPTH: u64 = 10000;

                while current_backtrack.header.previous_hash != common_ancestor_hash {
                    new_chain_segment.push(current_backtrack.clone());
                    let prev_hash = current_backtrack.header.previous_hash;

                    if let Some(prev_block) = self.get_block_by_hash(&prev_hash)? {
                        current_backtrack = prev_block;
                    } else {
                        return Err(anyhow!(
                            "CRITICAL: Missing block {} in fork chain during reorg preparation",
                            prev_hash
                        ));
                    }

                    depth += 1;
                    if depth > MAX_REORG_DEPTH {
                        return Err(anyhow!("Reorg depth exceeded limit of {}", MAX_REORG_DEPTH));
                    }
                }
                new_chain_segment.push(current_backtrack);
                new_chain_segment.reverse();

                self.reorganize_chain(new_chain_segment, common_ancestor_index)?;
                self.process_orphans_for_parent(new_block_hash)?;

                Ok(AddBlockResult::Added)
            } else {
                self.add_to_orphan_pool(new_block, new_block_hash)
            }
        }
    }

    pub fn add_direct_extension(
        &mut self,
        new_block: Block,
        new_block_hash: Hash,
    ) -> Result<AddBlockResult> {
        let expected_next_index = if self.get_tip_hash()?.is_some() {
            self.block_height()? + 1
        } else {
            0
        };

        let expected_next_target = self.calculate_next_target()?;

        if new_block.index != expected_next_index {
            return Ok(AddBlockResult::Rejected(format!(
                "Block {} has incorrect index. Expected {}, got {}",
                new_block_hash, expected_next_index, new_block.index
            )));
        }

        if let Err(e) = new_block.validate_block(self, &expected_next_target) {
            return Ok(AddBlockResult::Rejected(format!(
                "Block {} (index {}) failed validation: {}",
                new_block_hash, new_block.index, e
            )));
        }

        self.utxo_set.validate_block_utxos(&new_block)?;
        let simulated_utxos = self.utxo_set.simulate_apply(&new_block)?;

        let (new_supply, new_tx_count) = self
            .db
            .transaction(
                |tx_db| -> Result<(u64, u64), ConflictableTransactionError<ReorgError>> {
                    let initial_tx_count = self.total_tx_count;
                    let initial_supply = self.total_supply.as_smallest_unit();

                    let (new_supply, new_tx_count) = Self::apply_block_to_db(
                        tx_db,
                        &new_block,
                        &[],
                        initial_supply,
                        initial_tx_count,
                    )?;

                    let height_bytes = new_block.index.to_be_bytes().to_vec();
                    let hash_bytes = bincode::encode_to_vec(&new_block_hash, bincode_config())
                        .map_err(|e| ReorgError::Anyhow(e.into()))?;
                    tx_db.insert(DBKeys::CHAIN_HEIGHT, height_bytes)?;
                    tx_db.insert(DBKeys::TIP_HASH, hash_bytes)?;
                    let simulated_utxos_bytes =
                        bincode::encode_to_vec(&simulated_utxos, bincode_config())
                            .map_err(|e| ReorgError::Anyhow(e.into()))?;
                    tx_db.insert(DBKeys::PENDING_UTXO_SNAPSHOT, simulated_utxos_bytes)?;

                    Ok((new_supply, new_tx_count))
                },
            )
            .map_err(|e| match e {
                sled::transaction::TransactionError::Abort(ReorgError::Anyhow(err)) => err,
                sled::transaction::TransactionError::Storage(err) => anyhow::Error::from(err),
            })?;

        info!(
            "[DB] Database updated atomically for block {}.",
            new_block_hash
        );

        self.utxo_set = simulated_utxos;

        if let Some(pending_bytes) = self.db.get(DBKeys::PENDING_UTXO_SNAPSHOT)? {
            self.db
                .transaction(|tx_db| {
                    tx_db.insert(DBKeys::UTXO_SNAPSHOT, pending_bytes.clone())?;
                    tx_db.insert(
                        DBKeys::LAST_UTXO_SNAPSHOT_HEIGHT,
                        new_block.index.to_be_bytes().to_vec(),
                    )?;
                    tx_db.remove(DBKeys::PENDING_UTXO_SNAPSHOT)?;
                    Ok(())
                })
                .map_err(|e: sled::transaction::TransactionError| {
                    anyhow!("Failed to promote UTXO snapshot: {:?}", e)
                })?;
        } else {
            return Err(anyhow!(
                "CRITICAL: Pending UTXO snapshot disappeared after direct extension commit"
            ));
        }

        self.clear_mempool_of_block_transactions(&new_block, new_block_hash);
        self.target = expected_next_target;
        self.total_supply = Amount::from_smallest_unit(new_supply);
        self.total_tx_count = new_tx_count;
        self.tip_cache = Some((new_block_hash, new_block.clone()));

        if let Err(e) = self.verify_supply_integrity() {
            error!(
                "CRITICAL: Supply integrity check failed after adding block {}: {}",
                new_block_hash, e
            );
            return Err(e);
        }

        info!(
            "Block {} (index {}) accepted and added to chain. New height: {}",
            new_block_hash, new_block.index, new_block.index
        );

        self.daa_cache.insert(
            new_block.index,
            (new_block.header.timestamp, new_block.header.target),
        );
        self.prune_daa_cache(new_block.index);

        self.process_orphans_for_parent(new_block_hash)?;
        Ok(AddBlockResult::Added)
    }

    fn add_to_orphan_pool(&mut self, block: Block, block_hash: Hash) -> Result<AddBlockResult> {
        const MAX_ORPHAN_POOL_SIZE: usize = 1000;
        const MAX_ORPHAN_AGE_SECS: i64 = 3600;

        if self.orphan_cache_by_hash.contains_key(&block_hash) {
            info!("[ORPHAN] Ignoring already orphaned block {}", block_hash);
            return Ok(AddBlockResult::Orphaned);
        }

        let now = Utc::now();
        while let Some((insertion_time, _, _)) = self.orphan_order.front() {
            if now.signed_duration_since(*insertion_time).num_seconds() > MAX_ORPHAN_AGE_SECS {
                if let Some((_, oldest_orphan_hash, parent_of_oldest)) =
                    self.orphan_order.pop_front()
                {
                    warn!(
                        "[ORPHAN] Evicting stale orphan {} (older than {}s).",
                        oldest_orphan_hash, MAX_ORPHAN_AGE_SECS
                    );
                    if let Some(vec_for_parent) = self.orphan_pool.get_mut(&parent_of_oldest) {
                        vec_for_parent.retain(|(_, h, _)| *h != oldest_orphan_hash);
                        if vec_for_parent.is_empty() {
                            self.orphan_pool.remove(&parent_of_oldest);
                        }
                    }
                    self.orphan_cache_by_hash.remove(&oldest_orphan_hash);
                }
            } else {
                break;
            }
        }

        if self.orphan_cache_by_hash.len() >= MAX_ORPHAN_POOL_SIZE {
            warn!("[ORPHAN] Pool is full. Evicting oldest orphan to make space.");
            if let Some((_, oldest_orphan_hash, parent_of_oldest)) = self.orphan_order.pop_front() {
                if let Some(vec_for_parent) = self.orphan_pool.get_mut(&parent_of_oldest) {
                    vec_for_parent.retain(|(_, h, _)| *h != oldest_orphan_hash);
                    if vec_for_parent.is_empty() {
                        self.orphan_pool.remove(&parent_of_oldest);
                    }
                }
                self.orphan_cache_by_hash.remove(&oldest_orphan_hash);
            } else {
                return Ok(AddBlockResult::OrphanRejected(
                    "Orphan pool is full and could not evict an entry.".to_string(),
                ));
            }
        }

        let previous_hash = block.header.previous_hash;
        info!(
            "[ORPHAN] Adding block {} to orphan pool, waiting for parent {}",
            block_hash, previous_hash
        );
        self.orphan_pool
            .entry(previous_hash)
            .or_default()
            .push((now, block_hash, block));
        self.orphan_cache_by_hash.insert(block_hash, ());
        self.orphan_order
            .push_back((now, block_hash, previous_hash));

        Ok(AddBlockResult::Orphaned)
    }

    fn process_orphans_for_parent(&mut self, parent_hash: Hash) -> Result<()> {
        let mut parents_to_process = VecDeque::new();
        parents_to_process.push_back(parent_hash);

        while let Some(current_parent_hash) = parents_to_process.pop_front() {
            if let Some(orphans_to_process) = self.orphan_pool.remove(&current_parent_hash) {
                info!(
                    "[ORPHAN] Parent {} found. Processing {} orphan block(s).",
                    current_parent_hash,
                    orphans_to_process.len()
                );

                for (_timestamp, orphan_hash, orphan_block) in orphans_to_process {
                    self.orphan_cache_by_hash.remove(&orphan_hash);
                    self.orphan_order.retain(|(_, h, _)| *h != orphan_hash);

                    match self.add_block(orphan_block) {
                        Ok(AddBlockResult::Added) => {
                            parents_to_process.push_back(orphan_hash);
                        }
                        Ok(res) => warn!(
                            "[ORPHAN] Re-submitted orphan {} was not added: {:?}",
                            orphan_hash, res
                        ),
                        Err(e) => {
                            warn!(
                                "[ORPHAN] Error processing re-submitted orphan {}: {}",
                                orphan_hash, e
                            )
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn verify_supply_integrity(&self) -> Result<()> {
        let height = self.block_height()?;
        let expected_supply = crate::utils::calculate_expected_supply(height);

        if self.total_supply > expected_supply {
            return Err(anyhow!(
                "Total supply ({}) exceeds expected maximum ({}) at height {}. Inflation bug detected.",
                self.total_supply,
                expected_supply,
                height
            ));
        }

        Ok(())
    }

    pub fn save_snapshot_if_needed(&mut self, height: u64) -> Result<()> {
        if height > 0 && height % Self::UTXO_SNAPSHOT_INTERVAL == 0 {
            self.save_utxo_snapshot(height)?;
        }
        Ok(())
    }

    pub fn prune_daa_cache(&mut self, current_height: u64) {
        const CACHE_BUFFER: u64 = 100;
        let retain_after = current_height.saturating_sub(crate::DAA_WINDOW as u64 + CACHE_BUFFER);
        self.daa_cache.retain(|&index, _| index > retain_after);
    }

    fn find_common_ancestor(&self, previous_hash: &Hash) -> Option<(u64, Hash)> {
        const MAX_ANCESTOR_SEARCH_DEPTH: u32 = 720; // Approx. 24 hours.

        let mut current_hash = *previous_hash;

        for _ in 0..MAX_ANCESTOR_SEARCH_DEPTH {
            if let Ok(Some(ivec)) = self.db.get(DBKeys::hash_to_index(&current_hash)) {
                if ivec.len() == 8 {
                    let mut bytes = [0u8; 8];
                    bytes.copy_from_slice(&ivec);
                    let index = u64::from_be_bytes(bytes);
                    return Some((index, current_hash));
                } else {
                    warn!(
                        "Invalid hash_to_index length for {}: {} (expected 8). Attempting to walk back via stored block entry.",
                        current_hash,
                        ivec.len()
                    );
                }
            }

            if let Ok(Some(header)) = self.get_block_header_by_hash(&current_hash) {
                current_hash = header.previous_hash;
                if current_hash == Hash::zero() {
                    return None;
                }
            } else {
                return None;
            }
        }
        warn!("Ancestor search reached max depth without finding a common ancestor.");
        None
    }
}
