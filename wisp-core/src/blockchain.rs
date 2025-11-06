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
/// Represents the state of the blockchain.
/// NOTE: This struct is NOT thread-safe. Concurrent access must be managed externally, for example, using `Arc<RwLock<Blockchain>>`.
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
    pub mempool: HashMap<Hash, MempoolEntry>,
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
    /// Key: The `previous_hash` the orphan block is waiting for. Value: Vec of (insertion_time, orphan_hash, block)
    #[serde(skip)]
    pub orphan_pool: HashMap<Hash, Vec<(DateTime<Utc>, Hash, Block)>>,
    /// A quick lookup to check if an orphan block (by its own hash) is already in the pool.
    #[serde(skip)]
    pub orphan_cache_by_hash: HashMap<Hash, ()>,
    /// Deterministic order of orphan arrival for eviction (oldest-first).
    /// Each entry is (insertion_time, orphan_hash, parent_hash).
    #[serde(skip)]
    pub orphan_order: VecDeque<(DateTime<Utc>, Hash, Hash)>,
    /// An in-memory set of OutPoints that have been spent by transactions currently in the mempool.
    #[serde(skip)]
    pub mempool_spent_utxos: HashSet<OutPoint>,
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

    /// Returns a list of transactions from the mempool, ordered by fee (highest first)
    /// and then by timestamp (oldest first) for tie-breaking.
    /// This is suitable for inclusion in a block template.
    pub fn get_mempool_transactions_for_block(&self) -> Vec<Transaction> {
        let mut entries: Vec<MempoolEntry> = self.mempool.values().cloned().collect();
        // Sort by fee (descending), then by timestamp (ascending)
        entries.sort_by(|a, b| {
            b.fee
                .cmp(&a.fee)
                .then_with(|| a.timestamp.cmp(&b.timestamp))
        });
        entries.into_iter().map(|entry| entry.transaction).collect()
    }

    /// Creates a block template for a miner.
    ///
    /// The template includes the next block's index, the current PoW target, the hash of the
    /// current tip, and a selection of transactions from the mempool. It also creates a
    /// coinbase transaction that pays the block reward to the provided public key.
    ///
    /// The miner's job is to set a correct `timestamp` and find a `nonce` that satisfies the `target`.
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

        // 1. Select transactions from the mempool, respecting block size and tx count limits.
        let all_mempool_txs = self.get_mempool_transactions_for_block();
        let mut transactions_for_block = Vec::new();
        let mut total_fees = Amount::zero();
        let mut estimated_block_size = 0; // Start with a rough estimate

        // Pre-estimate the size of a block header and an empty transaction list.
        // This is a rough but effective way to account for the non-transaction data.
        let block_header_and_metadata_estimate = 256;
        estimated_block_size += block_header_and_metadata_estimate;

        for tx in all_mempool_txs {
            // Stop if we're about to exceed the transaction count limit.
            // We account for the coinbase transaction that will be added later.
            if transactions_for_block.len() + 1 >= crate::MAX_BLOCK_TRANSACTIONS {
                break;
            }

            let tx_size = bincode::encode_to_vec(&tx, bincode_config())?.len();

            // Stop if adding this transaction would exceed the block size limit.
            if estimated_block_size + tx_size > crate::MAX_BLOCK_SIZE_BYTES {
                break;
            }

            // If the transaction fits, add it to the block and update our running totals.
            let fee = self.calculate_transaction_fee(&tx)?;
            total_fees = total_fees.checked_add(fee).context("Fee sum overflow")?;
            estimated_block_size += tx_size;
            transactions_for_block.push(tx);
        }

        // 2. Create the coinbase transaction with the calculated fees.
        let block_reward = crate::utils::calculate_block_reward(index);
        let coinbase_value = block_reward
            .checked_add(total_fees)
            .context("Coinbase value overflow")?;

        let mut coinbase_data = index.to_le_bytes().to_vec();
        if let Some(msg) = coinbase_message {
            let msg_bytes = msg.as_bytes();
            // Ensure we don't exceed the max size for coinbase data (100 bytes total)
            if coinbase_data.len() + msg_bytes.len() <= 100 {
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

        // 3. Prepend the coinbase transaction to the list.
        transactions_for_block.insert(0, coinbase_tx);

        // 4. Calculate the final Merkle root.
        let merkle_root = crate::utils::MerkleRoot::calculate(&transactions_for_block)?;

        // 5. Construct and return the final block template.
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
        if transaction.is_coinbase() {
            return Ok(Amount::zero());
        }

        // Optimization: If the transaction is in the mempool, its fee is already calculated.
        let tx_hash = transaction.txid()?;
        if let Some(entry) = self.mempool.get(&tx_hash) {
            return Ok(entry.fee);
        }

        let outpoints_to_find: Vec<OutPoint> =
            transaction.inputs.iter().map(|i| i.outpoint).collect();
        let found_outputs = self.find_outputs_by_outpoints(&outpoints_to_find)?;

        let mut input_total = Amount::zero();
        for input in &transaction.inputs {
            if let Some(prev_output) = found_outputs.get(&input.outpoint) {
                // Use checked_add
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

        let output_total: Amount = transaction // Use sum()
            .outputs
            .iter()
            .map(|o| o.value)
            .sum();

        input_total
            .checked_sub(output_total)
            .context("Underflow calculating transaction fee (outputs > inputs)")
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

            // Before proceeding, do a basic sanity check on the header and PoW.
            // This prevents us from storing obvious junk or spam blocks.
            if let Err(e) = new_block.validate_header_and_pow() {
                return Ok(AddBlockResult::Rejected(format!(
                    "Invalid block header/PoW: {}",
                    e
                )));
            }

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

                // Save the block now that we know it's part of a potentially longer chain.
                // This prevents a race condition where a reorg is triggered for a block not yet on disk.
                let checked_block = CheckedBlock::from_block(new_block.clone())?;
                let checked_block_bytes = bincode::encode_to_vec(&checked_block, bincode_config())?;
                self.db
                    .insert(DBKeys::block(&new_block_hash), checked_block_bytes)?;
                info!(
                    "[FORK] Stored potential fork block {} (index {}) for future reorg.",
                    new_block_hash, new_block.index
                );

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
                // Attempt to add to the orphan pool first.
                let orphan_result = self.add_to_orphan_pool(new_block.clone(), new_block_hash)?;

                // Only save the block to disk if it was successfully added to the orphan pool.
                if let AddBlockResult::Orphaned = orphan_result {
                    let checked_block = CheckedBlock::from_block(new_block.clone())?;
                    let checked_block_bytes =
                        bincode::encode_to_vec(&checked_block, bincode_config())?;
                    self.db
                        .insert(DBKeys::block(&new_block_hash), checked_block_bytes)?;
                    info!("[ORPHAN] Stored orphan block {} to disk.", new_block_hash);
                }
                // If it was rejected, we don't save it, preventing DB pollution.
                // The `add_to_orphan_pool` function returns the appropriate `OrphanRejected` result.

                Ok(orphan_result)
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

        let (new_supply, new_tx_count) =
        // Atomically update the database with the new block and associated metadata.
        self.db
            .transaction(
                |tx_db| -> Result<(u64, u64), ConflictableTransactionError<ReorgError>> {
                    let initial_tx_count = self.total_tx_count;
                    let initial_supply = self.total_supply.as_smallest_unit();

                    // Use the shared helper to apply the block's DB changes.
                    let (new_supply, new_tx_count) = Self::apply_block_to_db(tx_db, &new_block, &[], initial_supply, initial_tx_count)?;

                    // Also update the chain height and tip hash atomically.
                    let height_bytes = new_block.index.to_be_bytes().to_vec();
                    let hash_bytes = bincode::encode_to_vec(&new_block_hash, bincode_config()).map_err(|e| ReorgError::Anyhow(e.into()))?;
                    tx_db.insert(DBKeys::CHAIN_HEIGHT, height_bytes)?;
                    tx_db.insert(DBKeys::TIP_HASH, hash_bytes)?;
                    // Store a pending UTXO snapshot to ensure atomicity with the block commit.
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

        // If the DB transaction was successful, commit the in-memory changes:
        // 1. Atomically update the in-memory UTXO set from the snapshot we just committed.
        // This is now the canonical in-memory state.
        self.utxo_set = simulated_utxos;

        // Promote the pending snapshot to the main snapshot for future rebuilds.
        // This is done atomically with removing the pending key.
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

        // Remove transactions from the mempool that were included in the block
        self.clear_mempool_of_block_transactions(&new_block, new_block_hash);

        // Update in-memory state to reflect the new tip. This is critical.
        self.target = expected_next_target;
        self.total_supply = Amount::from_smallest_unit(new_supply); // Correctly update from the transaction result
        self.total_tx_count = new_tx_count;

        // 4. Update the in-memory tip cache. This is the key fix for the template generation bug.
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
        const MAX_ORPHAN_AGE_SECS: i64 = 3600; // 1 hour

        if self.orphan_cache_by_hash.contains_key(&block_hash) {
            info!("[ORPHAN] Ignoring already orphaned block {}", block_hash);
            return Ok(AddBlockResult::Orphaned);
        }

        // If pool is full, evict the single oldest orphan block deterministically.
        // Also, periodically prune any very old orphans.
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
                // Remove the orphan from the parent's list
                if let Some(vec_for_parent) = self.orphan_pool.get_mut(&parent_of_oldest) {
                    vec_for_parent.retain(|(_, h, _)| *h != oldest_orphan_hash);
                    if vec_for_parent.is_empty() {
                        self.orphan_pool.remove(&parent_of_oldest);
                    }
                }
                // Remove from cache
                self.orphan_cache_by_hash.remove(&oldest_orphan_hash);
            } else {
                // No entry to evict (shouldn't happen) - return rejection
                return Ok(AddBlockResult::OrphanRejected(
                    "Orphan pool is full and could not evict an entry.".to_string(),
                ));
            }
        }

        let previous_hash = block.previous_hash;
        info!(
            "[ORPHAN] Adding block {} to orphan pool, waiting for parent {}",
            block_hash, previous_hash
        );
        self.orphan_pool
            .entry(previous_hash)
            .or_default()
            .push((now, block_hash, block));
        self.orphan_cache_by_hash.insert(block_hash, ());
        // push to deterministic order queue
        self.orphan_order
            .push_back((now, block_hash, previous_hash));

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
                for (_timestamp, orphan_hash, orphan_block) in orphans_to_process {
                    self.orphan_cache_by_hash.remove(&orphan_hash);
                    // Also remove from the deterministic order queue
                    self.orphan_order.retain(|(_, h, _)| *h != orphan_hash);

                    // Re-submit the orphan block to the main `add_block` flow.
                    // We match on the result to avoid `?` propagating an error and stopping
                    // the processing of other potential orphans for this parent.
                    match self.add_block(orphan_block) {
                        Ok(AddBlockResult::Added) => {
                            // The newly added block becomes the parent for the next iteration.
                            current_parent_hash = orphan_hash;
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
        const MAX_ANCESTOR_SEARCH_DEPTH: u32 = 720; // Approx. 24 hours.

        let mut current_hash = *previous_hash;

        for _ in 0..MAX_ANCESTOR_SEARCH_DEPTH {
            // Check if the current hash exists in our main chain.
            // If it does, we've found the common ancestor.
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
                    // fall through to attempt get_block_by_hash below (don't `continue`, so code will try DB block fetch)
                }
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
