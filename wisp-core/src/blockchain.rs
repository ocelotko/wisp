use crate::{
    currency::Amount,
    reorg::ReorgError,
    sha256::{hash, Hash, Hashable, Sha256},
    storage::DBKeys,
    transactions::{OutPoint, Transaction, TransactionOutput},
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
/// Represents the possible outcomes of attempting to add a new block to the blockchain.
#[derive(Debug)]
pub enum AddBlockResult {
    /// The block was successfully added to the main chain.
    Added,
    /// The block was rejected due to a validation error or other issue.
    Rejected(String),
    /// A fork was detected where the new block could potentially lead to a longer chain.
    /// This signals the need for a chain reorganization process.
    PotentialLongerForkDetected {
        common_ancestor_index: u64,
        new_block_index: u64,
        new_block_hash: Hash,
    },
    /// The block's parent is unknown. The block has been added to the orphan pool.
    Orphaned,
    /// The block's parent is unknown and it was rejected from the orphan pool (e.g., pool is full).
    OrphanRejected(String),
    /// The block is part of a fork that is shorter than the current main chain and was rejected.
    ShorterForkRejected(String),
}

/// Represents a block in the blockchain.
#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// The version of the block structure.
    pub version: u32,
    /// The timestamp when the block was created.
    #[bincode(with_serde)]
    pub timestamp: DateTime<Utc>,
    /// A random number used in the proof-of-work algorithm.
    pub nonce: u64,
    /// The hash of the preceding block in the chain.
    pub previous_hash: Hash,
    /// The root of the Merkle tree of transactions included in this block.
    pub merkle_root: MerkleRoot,
    /// The proof-of-work target for this block. The block's hash must be less than or equal to this value.
    pub target: U256,
    /// The height of the block in the chain (0 for the genesis block).
    pub index: u64,
    /// The list of transactions included in the block.
    pub transactions: Vec<Transaction>,
}

/// A wrapper for a block that includes a checksum to verify data integrity upon deserialization.
#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct CheckedBlock {
    block: Block,
    checksum: Hash,
}

impl CheckedBlock {
    /// Creates a new `CheckedBlock` from a `Block`, calculating its checksum.
    pub(crate) fn from_block(block: Block) -> Result<Self> {
        let block_bytes = bincode::encode_to_vec(&block, bincode_config())?;
        let checksum = hash(&block_bytes[..]);
        Ok(Self { block, checksum })
    }

    /// Verifies the block's integrity and returns the inner block if valid.
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
            version,
            timestamp,
            nonce,
            previous_hash,
            merkle_root,
            target,
            index,
            transactions,
        }
    }

    /// Creates a `BlockHeader` for this block, used for hashing and validation.
    /// This is a lightweight, temporary struct that borrows data from the full block.
    pub fn header(&self) -> BlockHeader {
        BlockHeader {
            version: self.version,
            timestamp: self.timestamp,
            nonce: self.nonce,
            previous_hash: self.previous_hash,
            merkle_root: self.merkle_root,
            target: self.target,
        }
    }

    /// Calculates the hash of the block header, which serves as the block's unique identifier (txid).
    pub fn id(&self) -> Result<Hash, anyhow::Error> {
        Ok(hash(&self.header()))
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Blockchain {
    /// The in-memory set of Unspent Transaction Outputs (UTXOs).
    pub utxo_set: UtxoSet,
    /// The current proof-of-work target for the next block.
    pub target: U256,
    /// The database handle for persistent storage. It is not serialized.
    #[serde(skip)]
    pub db: Db,
    /// The memory pool of unconfirmed transactions.
    #[serde(default)]
    pub mempool: HashMap<Hash, (DateTime<Utc>, Transaction, Amount)>,
    /// An in-memory cache for recent block headers to speed up DAA calculations.
    #[serde(skip)]
    pub daa_cache: HashMap<u64, (DateTime<Utc>, U256)>,
    /// An in-memory cache for the current tip block to reduce DB reads.
    #[serde(skip)]
    pub tip_cache: Option<(Hash, Block)>,
    /// In-memory cache for the total circulating supply.
    #[serde(skip)]
    pub total_supply: Amount,
    /// In-memory cache for the total number of confirmed transactions.
    #[serde(skip)]
    pub total_tx_count: u64,
    /// A pool to store blocks whose parents have not yet been received.
    /// Key: The `previous_hash` the orphan block is waiting for. Value: The orphan block itself.
    #[serde(skip)]
    pub orphan_pool: HashMap<Hash, Vec<Block>>,
    /// A quick lookup to check if an orphan block (by its own hash) is already in the pool.
    #[serde(skip)]
    pub orphan_cache_by_hash: HashMap<Hash, ()>,
}

impl Blockchain {
    /// The interval (in number of blocks) at which to save a UTXO snapshot.
    /// 720 blocks * 2 minutes/block = 1440 minutes = 24 hours.
    pub const UTXO_SNAPSHOT_INTERVAL: u64 = 720;

    /// Creates a new `Blockchain` instance with a database connection.
    /// Note: This creates an empty, in-memory instance. `load_from_db` must be called to initialize state.
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
        }
    }

    pub fn utxos(&self) -> &HashMap<OutPoint, (bool, TransactionOutput)> {
        &self.utxo_set.utxos
    }

    pub fn mempool(&self) -> &HashMap<Hash, (DateTime<Utc>, Transaction, Amount)> {
        &self.mempool
    }

    /// Returns a list of transactions from the mempool, ordered by fee (highest first)
    /// and then by timestamp (oldest first) for tie-breaking.
    /// This is suitable for inclusion in a block template.
    pub fn get_mempool_transactions_for_block(&self) -> Vec<Transaction> {
        let mut transactions: Vec<(DateTime<Utc>, Transaction, Amount)> =
            self.mempool.values().cloned().collect();
        // Sort by fee (descending), then by timestamp (ascending)
        transactions.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        transactions.into_iter().map(|(_, tx, _)| tx).collect()
    }

    pub fn get_target(&self) -> U256 {
        self.target
    }

    pub fn calculate_transaction_fee(&self, transaction: &Transaction) -> Result<Amount> {
        if transaction.is_coinbase() {
            return Ok(Amount::zero());
        }

        let outpoints_to_find: Vec<OutPoint> =
            transaction.inputs.iter().map(|i| i.outpoint).collect();
        let found_outputs = self.find_outputs_by_outpoints(&outpoints_to_find)?;

        let mut input_total = Amount::zero();
        for input in &transaction.inputs {
            if let Some(prev_output) = found_outputs.get(&input.outpoint) {
                input_total = (input_total + prev_output.value)
                    .context("Overflow calculating total input value")?;
            } else {
                return Err(anyhow!(
                    "Input UTXO {} not found for fee calculation in transaction {}",
                    input.outpoint,
                    transaction.txid().unwrap_or_default()
                ));
            }
        }

        let output_total = transaction
            .outputs
            .iter()
            .try_fold(Amount::zero(), |acc, o| acc + o.value)?;

        input_total - output_total
    }

    /// Adds a new block to the blockchain.
    /// This is the main entry point for processing new blocks from the network.
    /// It handles validation, direct chain extensions, and fork detection.
    pub fn add_block(&mut self, new_block: Block) -> Result<AddBlockResult> {
        // First, calculate the hash of the new block.
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

        // Explicitly get the current tip block to make logic clearer.
        let maybe_current_tip = self.get_tip_block()?;

        let current_chain_tip = if let Some(block) = maybe_current_tip {
            block
        } else {
            // Chain is empty. We must be processing the genesis block.
            if new_block.index == 0 && new_block.previous_hash == Hash::zero() {
                info!("[CHAIN] Chain is empty, processing genesis block.");
                return self.add_direct_extension(new_block, new_block_hash);
            } else {
                return Ok(AddBlockResult::OrphanRejected(
                    "Chain is not initialized. Cannot accept non-genesis peer blocks.".to_string(),
                ));
            }
        };
        let current_chain_tip_hash = current_chain_tip.id()?;

        // Case 1: The new block is a direct extension of our current chain.
        if new_block.previous_hash == current_chain_tip_hash {
            info!(
                "[CHAIN] New block {} is a direct extension of the current tip.",
                new_block_hash
            );
            return self.add_direct_extension(new_block, new_block_hash);
        // Case 2: The new block is not a direct extension, indicating a fork or an out-of-order block.
        } else {
            warn!(
                "[FORK] Fork detected or out-of-order block ({}). Local tip: {} (index {}), New block previous: {} (index {})",
                new_block_hash,
                current_chain_tip_hash, current_chain_tip.index,
                new_block.previous_hash, new_block.index.saturating_sub(1)
            );

            // Before determining the fork type, save the block. This prevents a race condition
            // where the reorg process is triggered but the block it needs is not yet on disk.
            let checked_block = CheckedBlock::from_block(new_block.clone())?;
            let checked_block_bytes = bincode::encode_to_vec(&checked_block, bincode_config())?;
            // Use the canonical DB key so get_block_by_hash can find this block later.
            self.db
                .insert(DBKeys::block(&new_block_hash), checked_block_bytes)?;
            info!(
                "[FORK] Stored potential fork block {} for future reorg.",
                new_block_hash
            );

            // Try to find a common ancestor between our chain and the new block's chain.
            if let Some((common_ancestor_index, common_ancestor_hash)) =
                self.find_common_ancestor(&new_block.previous_hash)
            {
                info!(
                    "[FORK] Found common ancestor {} at index {} for received block {}",
                    common_ancestor_hash, common_ancestor_index, new_block_hash
                );

                // If the new block's index is not higher than our current tip, it can't be a longer chain.
                if new_block.index <= current_chain_tip.index {
                    return Ok(AddBlockResult::ShorterForkRejected(format!(
                        "Received block {} is part of a shorter or equal length fork (new index {} <= current index {}). Rejecting.",
                        new_block_hash, new_block.index, current_chain_tip.index,
                    )));
                }

                // Before signaling a reorg, atomically save the state required to recover if we crash.
                self.db
                    .transaction(|tx_db| {
                        let tip_bytes = bincode::encode_to_vec(&new_block_hash, bincode_config())
                            .map_err(|e| ReorgError::Anyhow(e.into()))?;
                        let ancestor_bytes =
                            bincode::encode_to_vec(&common_ancestor_index, bincode_config())
                                .map_err(|e| ReorgError::Anyhow(e.into()))?;
                        let ancestor_hash_bytes =
                            bincode::encode_to_vec(&common_ancestor_hash, bincode_config())
                                .map_err(|e| ReorgError::Anyhow(e.into()))?;

                        tx_db.insert(DBKeys::PENDING_REORG_TIP, tip_bytes)?;
                        tx_db.insert(DBKeys::PENDING_REORG_ANCESTOR, ancestor_bytes)?;
                        tx_db.insert(DBKeys::PENDING_REORG_ANCESTOR_HASH, ancestor_hash_bytes)?;
                        Ok(())
                    })
                    .map_err(|e| anyhow!("Failed to save pending reorg state: {:?}", e))?;

                // The new block is on a potentially longer fork. Signal this for a reorg.
                Ok(AddBlockResult::PotentialLongerForkDetected {
                    common_ancestor_index,
                    new_block_index: new_block.index,
                    new_block_hash,
                })
            } else {
                // No common ancestor found, the block is an orphan.
                // The block was already saved to disk, so we just need to add it to the orphan pool.
                self.add_to_orphan_pool(new_block.clone(), new_block_hash)
            }
        }
    }

    /// Handles the addition of a block that directly extends the current main chain.
    /// This involves validation, database updates, and in-memory state changes.
    pub fn add_direct_extension(
        &mut self,
        new_block: Block,
        new_block_hash: Hash,
    ) -> Result<AddBlockResult> {
        let expected_next_index = if self.get_tip_hash()?.is_some() {
            self.block_height()? + 1
        } else {
            0 // Expecting genesis block
        };

        let expected_next_target = self.calculate_next_target()?;

        // Validate the block's index.
        if new_block.index != expected_next_index {
            return Ok(AddBlockResult::Rejected(format!(
                "Block {} has incorrect index. Expected {}, got {}",
                new_block_hash, expected_next_index, new_block.index
            )));
        }

        // Perform full block validation (PoW, Merkle root, transactions, etc.).
        if let Err(e) = new_block.validate_block(self, &expected_next_target) {
            return Ok(AddBlockResult::Rejected(format!(
                "Block {} (index {}) failed validation: {}",
                new_block_hash, new_block.index, e
            )));
        }

        // Pre-validate UTXO application to ensure it won't fail mid-transaction.
        self.utxo_set.validate_block_utxos(&new_block)?;
        // Create a snapshot of what the UTXO set will look like after applying the new block.
        let simulated_utxos = self.utxo_set.simulate_apply(&new_block)?;

        // Atomically update the database with the new block and associated metadata.
        self.db
            .transaction(
                |tx_db| -> Result<(), ConflictableTransactionError<ReorgError>> {
                    let mut new_tx_count = self.total_tx_count;
                    let mut new_supply = self.total_supply.as_smallest_unit();

                    // Use the shared helper to apply the block's DB changes.
                    self.apply_block_to_db(
                        tx_db,
                        &new_block,
                        &[],
                        &mut new_supply,
                        &mut new_tx_count,
                    )?;

                    // Store a pending UTXO snapshot to ensure atomicity with the block commit.
                    let simulated_utxos_bytes =
                        bincode::encode_to_vec(&simulated_utxos, bincode_config())
                            .map_err(|e| ReorgError::Anyhow(e.into()))?;
                    tx_db.insert(DBKeys::PENDING_UTXO_SNAPSHOT, simulated_utxos_bytes)?;

                    Ok(())
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

        // If the DB transaction was successful, commit the in-memory changes:
        // 1. Atomically update the in-memory UTXO set from the snapshot we just committed.
        // This is now the canonical in-memory state.
        self.utxo_set = simulated_utxos;

        // 2. Promote the pending snapshot to the main snapshot for future rebuilds.
        // This is done atomically with removing the pending key.
        let pending_bytes = self.db.get(DBKeys::PENDING_UTXO_SNAPSHOT)?.ok_or_else(|| {
            anyhow!("CRITICAL: Pending UTXO snapshot disappeared after direct extension commit")
        })?;
        self.db.insert(DBKeys::UTXO_SNAPSHOT, pending_bytes)?;
        self.db.remove(DBKeys::PENDING_UTXO_SNAPSHOT)?; // Now it's safe to remove
        self.db.insert(
            DBKeys::LAST_UTXO_SNAPSHOT_HEIGHT,
            new_block.index.to_be_bytes().to_vec(),
        )?;

        // 2. Remove transactions from the mempool that were included in the block
        self.clear_mempool_of_block_transactions(&new_block, new_block_hash);

        // 3. Update in-memory state from the database to ensure consistency after the commit.
        self.target = expected_next_target;
        self.total_supply = Amount::from_smallest_unit(self.get_total_supply_from_db()?);
        self.total_tx_count = self.get_total_transaction_count_from_db()?;

        // 4. Update the in-memory tip cache.
        self.tip_cache = Some((new_block_hash, new_block.clone()));

        // This log provides clear, consistent confirmation when a block is added.
        info!(
            "✅ Block {} (index {}) accepted and added to chain. New height: {}",
            new_block_hash, new_block.index, new_block.index
        );

        // Update the DAA cache with the new block's data and prune old entries.
        self.daa_cache
            .insert(new_block.index, (new_block.timestamp, new_block.target));
        self.prune_daa_cache(new_block.index);

        // After adding a block, check if it resolves any orphans.
        self.process_orphans_for_parent(new_block_hash)?;
        Ok(AddBlockResult::Added)
    }

    /// Adds a block to the orphan pool.
    fn add_to_orphan_pool(&mut self, block: Block, block_hash: Hash) -> Result<AddBlockResult> {
        const MAX_ORPHAN_POOL_SIZE: usize = 1000;

        if self.orphan_cache_by_hash.contains_key(&block_hash) {
            info!("[ORPHAN] Ignoring already orphaned block {}", block_hash);
            return Ok(AddBlockResult::Orphaned);
        }

        if self.orphan_cache_by_hash.len() >= MAX_ORPHAN_POOL_SIZE {
            warn!("[ORPHAN] Pool is full. Evicting an orphan to make space.");
            // Simple eviction: remove the first entry. A more advanced strategy could be used.
            if let Some(key_to_remove) = self.orphan_pool.keys().next() {
                let key_clone = *key_to_remove;
                if let Some(removed_orphans) = self.orphan_pool.remove(&key_clone) {
                    for orphan_to_evict in removed_orphans {
                        match orphan_to_evict.id() {
                            Ok(orphan_hash) => {
                                self.orphan_cache_by_hash.remove(&orphan_hash);
                            }
                            Err(e) => {
                                error!("[ORPHAN] Failed to get ID of orphan being evicted. Cache may be inconsistent. Error: {}", e);
                            }
                        }
                    }
                }
            } else {
                return Ok(AddBlockResult::OrphanRejected(
                    "Orphan pool is full and could not evict an entry.".to_string(),
                ));
            }
        }

        info!(
            "[ORPHAN] Adding block {} to orphan pool, waiting for parent {}",
            block_hash, block.previous_hash
        );
        self.orphan_pool
            .entry(block.previous_hash)
            .or_default()
            .push(block);
        self.orphan_cache_by_hash.insert(block_hash, ());

        Ok(AddBlockResult::Orphaned)
    }

    /// After a block is added, this function checks if it's the parent of any orphans and tries to process them.
    fn process_orphans_for_parent(&mut self, parent_hash: Hash) -> Result<()> {
        let mut current_parent_hash = parent_hash;
        // Use a loop instead of recursion to prevent stack overflow when processing a long chain of orphans.
        loop {
            if let Some(orphans_to_process) = self.orphan_pool.remove(&current_parent_hash) {
                info!(
                    "[ORPHAN] Parent {} found. Processing {} orphan block(s).",
                    current_parent_hash,
                    orphans_to_process.len()
                );
                // In most cases, there will only be one orphan per parent.
                // If there are multiple, we process the first one and the rest will be re-processed
                // if the first one is successfully added.
                for orphan in orphans_to_process {
                    let orphan_hash_result = orphan.id();
                    if let Ok(orphan_hash) = orphan_hash_result {
                        self.orphan_cache_by_hash.remove(&orphan_hash);
                        // Re-submit the orphan block to the main `add_block` flow.
                        // We match on the result to avoid `?` propagating an error and stopping
                        // the processing of other potential orphans for this parent.
                        match self.add_block(orphan) {
                            Ok(AddBlockResult::Added) => {
                                // The newly added block becomes the parent for the next iteration.
                                current_parent_hash = orphan_hash;
                            }
                            Ok(res) => warn!(
                                "[ORPHAN] Re-submitted orphan {} was not added: {:?}",
                                orphan_hash, res
                            ),
                            Err(e) => warn!(
                                "[ORPHAN] Error processing re-submitted orphan {}: {}",
                                orphan_hash, e
                            ),
                        }
                    }
                }
            } else {
                // No more orphans found for the current parent, so we can stop.
                break;
            }
        }
        Ok(())
    }

    /// Periodically saves a UTXO snapshot to disk to speed up future startups.
    pub fn maybe_save_snapshot(&self, height: u64) -> Result<()> {
        if height > 0 && height % 720 == 0 {
            self.save_utxo_snapshot(height)?;
        }
        Ok(())
    }

    /// Prunes the DAA cache to keep it from growing indefinitely.
    /// It retains entries only within the DAA window and a small buffer.
    pub fn prune_daa_cache(&mut self, current_height: u64) {
        const CACHE_BUFFER: u64 = 100; // Keep a bit more than the DAA window
        let retain_after = current_height.saturating_sub(crate::DAA_WINDOW as u64 + CACHE_BUFFER);
        self.daa_cache.retain(|&index, _| index > retain_after);
    }

    /// Finds the common ancestor of a potential fork by walking back from the current tip.
    /// This optimized version uses the `hash_to_index` DB lookup to efficiently walk backwards.
    fn find_common_ancestor(&self, previous_hash: &Hash) -> Option<(u64, Hash)> {
        const MAX_ANCESTOR_SEARCH_DEPTH: u32 = 2016; // Approx. 2 weeks, a reasonable limit.

        let mut current_hash = *previous_hash;

        for _ in 0..MAX_ANCESTOR_SEARCH_DEPTH {
            // Check if the current hash exists in our main chain.
            // If it does, we've found the common ancestor.
            if let Ok(Some(ivec)) = self.db.get(DBKeys::hash_to_index(&current_hash)) {
                if ivec.len() != 8 {
                    warn!(
                        "Invalid hash_to_index length for {}: {} (expected 8). Skipping.",
                        current_hash,
                        ivec.len()
                    );
                    return None;
                }
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&ivec);
                let index = u64::from_be_bytes(bytes);
                return Some((index, current_hash));
            }

            // If not, get the block from the DB (it might be a fork block we've stored)
            // and walk back to its predecessor.
            if let Ok(Some(block)) = self.get_block_by_hash(&current_hash) {
                current_hash = block.previous_hash;
                if current_hash == Hash::zero() {
                    // We've walked back to before genesis, which means no common ancestor was found in our chain.
                    return None;
                }
            } else {
                // The block isn't in our DB at all, so we can't trace it back further.
                return None;
            }
        }
        warn!("Ancestor search reached max depth without finding a common ancestor.");
        None // Reached max depth
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{transactions::Transaction, utils};
    use tempfile::tempdir;

    // Helper to create a temporary DB and a Blockchain instance for testing.
    fn setup_test_blockchain() -> (Blockchain, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db = sled::open(dir.path()).unwrap();
        let blockchain = Blockchain::new(db);
        (blockchain, dir)
    }

    // Helper to create a new block that builds on a previous one.
    fn create_next_block(
        blockchain: &mut Blockchain,
        prev_block: &Block,
        transactions: Vec<Transaction>,
    ) -> Block {
        let index = prev_block.index + 1;
        let prev_block_hash = prev_block.id().unwrap();

        // Ensure the timestamp is always greater than the previous block's to pass MTP validation.
        // This also helps ensure mining progress in tests.
        let timestamp = prev_block.timestamp + chrono::Duration::seconds(1);

        // A block must always have a coinbase transaction.
        let mut block_transactions = transactions;
        let coinbase_output = TransactionOutput {
            value: utils::calculate_block_reward(index),
            pubkey: utils::genesis_block().unwrap().transactions[0].outputs[0]
                .pubkey
                .clone(),
        };
        let coinbase_input = crate::transactions::TransactionInput {
            outpoint: crate::transactions::OutPoint {
                txid: Hash::zero(),
                vout: u32::MAX,
            },
            coinbase_data: Some(index.to_le_bytes().to_vec()),
            signature: None,
        };
        let coinbase_tx = Transaction::new(vec![coinbase_input], vec![coinbase_output]);
        block_transactions.insert(0, coinbase_tx);

        let merkle_root = utils::MerkleRoot::calculate(&block_transactions).unwrap();

        // CRITICAL: Calculate the target based on the PREVIOUS block's height, not the current
        // tip of the main blockchain instance. This is essential for creating valid fork blocks in tests.
        let target = blockchain
            .calculate_next_target_from_height(prev_block.index)
            .unwrap();
        let mut new_block = Block::new(
            1,
            timestamp,
            0, // nonce starts at 0
            prev_block_hash,
            merkle_root,
            target,
            index,
            block_transactions,
        );
        // Mine the block until it's valid.
        new_block.mine_block(1_000_000).unwrap();
        new_block
    }

    #[test]
    fn test_add_genesis_block() {
        let (mut blockchain, _dir) = setup_test_blockchain();
        let genesis_block = utils::genesis_block().unwrap();

        let result = blockchain.add_block(genesis_block.clone()).unwrap();
        assert!(matches!(result, AddBlockResult::Added));
        assert_eq!(blockchain.block_height().unwrap(), 0);
        assert_eq!(
            blockchain.get_tip_hash().unwrap().unwrap(),
            genesis_block.id().unwrap()
        );
    }

    #[test]
    fn test_add_valid_second_block() {
        let (mut blockchain, _dir) = setup_test_blockchain();
        let genesis_block = utils::genesis_block().unwrap();
        blockchain.add_block(genesis_block.clone()).unwrap();

        let stored_genesis = blockchain.get_tip_block().unwrap().unwrap();
        let second_block = create_next_block(&mut blockchain, &stored_genesis, vec![]);
        let result = blockchain.add_block(second_block.clone()).unwrap();

        assert!(matches!(result, AddBlockResult::Added));
        assert_eq!(blockchain.block_height().unwrap(), 1);
        assert_eq!(
            blockchain.get_tip_hash().unwrap().unwrap(),
            second_block.id().unwrap()
        );
    }

    #[test]
    fn test_reject_block_with_bad_index() {
        let (mut blockchain, _dir) = setup_test_blockchain();
        let genesis_block = utils::genesis_block().unwrap();
        blockchain.add_block(genesis_block.clone()).unwrap();

        let mut bad_block = create_next_block(&mut blockchain, &genesis_block, vec![]);
        bad_block.index = 3; // Invalid index

        let result = blockchain.add_block(bad_block).unwrap();
        assert!(matches!(result, AddBlockResult::Rejected(_)));
        assert_eq!(
            blockchain.block_height().unwrap(),
            0,
            "Chain height should not change on rejection"
        );
    }

    #[test]
    fn test_reject_shorter_fork() {
        let (mut blockchain, _dir) = setup_test_blockchain();
        let genesis_block = utils::genesis_block().unwrap();
        blockchain.add_block(genesis_block.clone()).unwrap();

        // Main chain has one block after genesis
        let stored_genesis = blockchain.get_tip_block().unwrap().unwrap();
        let main_chain_block_1 = create_next_block(&mut blockchain, &stored_genesis, vec![]);
        blockchain.add_block(main_chain_block_1.clone()).unwrap();
        assert_eq!(blockchain.block_height().unwrap(), 1);

        // A competing block is mined, also based on genesis. This creates a fork of equal length.
        let fork_block_1 = create_next_block(&mut blockchain, &stored_genesis, vec![]);

        // The node should reject this block because it doesn't create a longer chain.
        let result = blockchain.add_block(fork_block_1).unwrap();
        assert!(matches!(result, AddBlockResult::ShorterForkRejected(_)));
        assert_eq!(
            blockchain.block_height().unwrap(),
            1,
            "Chain height should not change when rejecting a shorter/equal fork"
        );
    }

    #[test]
    fn test_detect_longer_fork() {
        let (mut blockchain, _dir) = setup_test_blockchain();
        let genesis_block = utils::genesis_block().unwrap();
        blockchain.add_block(genesis_block.clone()).unwrap();

        // Main chain has one block after genesis
        let stored_genesis = blockchain.get_tip_block().unwrap().unwrap();
        let main_chain_block_1 = create_next_block(&mut blockchain, &stored_genesis, vec![]);
        blockchain.add_block(main_chain_block_1.clone()).unwrap();
        assert_eq!(blockchain.block_height().unwrap(), 1);

        // Now, a fork appears that is longer.
        // Fork Block 1 (builds on genesis)
        let fork_block_1 = create_next_block(&mut blockchain, &stored_genesis, vec![]);
        // Fork Block 2 (builds on Fork Block 1) - Pass a clone of blockchain to avoid move
        let fork_block_2 = create_next_block(&mut blockchain, &fork_block_1, vec![]);

        // When we add the first block of the fork, it should be stored but not change the main chain.
        // It's a shorter/equal fork at this point.
        let result1 = blockchain.add_block(fork_block_1).unwrap();
        assert!(matches!(result1, AddBlockResult::ShorterForkRejected(_)));

        // When we add the second block, our node should recognize it's part of a longer chain.
        let result2 = blockchain.add_block(fork_block_2).unwrap();
        assert!(matches!(
            result2,
            AddBlockResult::PotentialLongerForkDetected { .. }
        ));
    }

    #[test]
    fn test_successful_reorg() {
        let (mut blockchain, _dir) = setup_test_blockchain();
        let genesis_block = utils::genesis_block().unwrap();
        blockchain.add_block(genesis_block.clone()).unwrap();

        // 1. Create a main chain of length 1 (total height 1)
        let stored_genesis = blockchain.get_tip_block().unwrap().unwrap();
        let main_chain_block_1 = create_next_block(&mut blockchain, &stored_genesis, vec![]);
        blockchain.add_block(main_chain_block_1.clone()).unwrap();
        assert_eq!(blockchain.block_height().unwrap(), 1);
        assert_eq!(
            blockchain.get_tip_hash().unwrap().unwrap(),
            main_chain_block_1.id().unwrap()
        );

        // 2. Create a longer competing fork of length 2 (total height 2)
        let fork_block_1 = create_next_block(&mut blockchain, &stored_genesis, vec![]);
        let fork_block_2 = create_next_block(&mut blockchain, &fork_block_1, vec![]);

        // 3. Add the fork blocks. The second one should trigger a reorg signal.
        blockchain.add_block(fork_block_1.clone()).unwrap();
        let result = blockchain.add_block(fork_block_2.clone()).unwrap();

        // 4. Execute the reorganization
        if let AddBlockResult::PotentialLongerForkDetected {
            common_ancestor_index,
            ..
        } = result
        {
            blockchain
                .reorganize_chain(
                    vec![fork_block_1, fork_block_2.clone()],
                    common_ancestor_index,
                )
                .unwrap();
        }

        // 5. Assert that the chain has successfully switched to the new tip.
        assert_eq!(
            blockchain.block_height().unwrap(),
            fork_block_2.index,
            "Chain height should be updated after reorg"
        );
        assert_eq!(
            blockchain.get_tip_hash().unwrap().unwrap(),
            fork_block_2.id().unwrap(),
            "Chain tip should be the new fork's tip after reorg"
        );
    }

    #[test]
    fn test_fork_block_is_persisted() {
        let (mut blockchain, _dir) = setup_test_blockchain();
        let genesis_block = utils::genesis_block().unwrap();
        blockchain.add_block(genesis_block.clone()).unwrap();

        // Create a main chain block
        let stored_genesis = blockchain.get_tip_block().unwrap().unwrap();
        let main_chain_block_1 = create_next_block(&mut blockchain, &stored_genesis, vec![]);
        blockchain.add_block(main_chain_block_1.clone()).unwrap();

        // Create a competing fork block that also builds on genesis
        let fork_block_1 = create_next_block(&mut blockchain, &stored_genesis, vec![]);
        let fork_hash = fork_block_1.id().unwrap();

        // Add the fork block. It should be rejected as a shorter fork, but it must be persisted first.
        let result = blockchain.add_block(fork_block_1).unwrap();
        assert!(matches!(result, AddBlockResult::ShorterForkRejected(_)));

        // CRITICAL: Verify that the fork block was actually saved to the DB,
        // even though it wasn't adopted as the main chain tip.
        // This is essential for the `find_common_ancestor_by_hash` logic to work.
        let persisted_fork_block = blockchain.get_block_by_hash(&fork_hash).unwrap();
        assert!(
            persisted_fork_block.is_some(),
            "Fork block should be persisted in the database even if not adopted"
        );
    }
}
