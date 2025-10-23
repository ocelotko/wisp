use crate::{
    currency::Amount,
    reorg::ReorgError,
    sha256::Hash,
    transactions::{OutPoint, Transaction, TransactionOutput},
    utils::{MerkleRoot, Saveable},
    utxo::UtxoSet,
    U256,
};
use anyhow::anyhow;
use anyhow::Result;
use chrono::{DateTime, Utc};
use log::{error, info, warn};
use serde::Deserialize;
use serde::Serialize;
use serde_with::serde_as;
use sha2::Digest;
use sled::transaction::ConflictableTransactionError;
use sled::Db;
use std::collections::HashMap;
use std::io::{Error as IoError, ErrorKind as IoErrorKind, Read, Result as IoResult, Write};

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
    /// The block's previous hash does not correspond to any known block in the chain, making it an orphan.
    OrphanedOrDisconnected(String),
    /// The block is part of a fork that is shorter than the current main chain and was rejected.
    ShorterForkRejected(String),
}

/// Represents a block in the blockchain.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Block {
    /// The version of the block structure.
    pub version: u32,
    /// The timestamp when the block was created.
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

#[derive(Serialize)]
pub struct BlockHeader {
    pub version: u32,
    pub timestamp: DateTime<Utc>,
    pub nonce: u64,
    pub previous_hash: Hash,
    pub merkle_root: MerkleRoot,
    pub target: U256,
}

use crate::sha256::{hash, Hashable};
use sha2::Sha256;

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

impl Saveable for Block {
    fn load<I: Read>(reader: I) -> IoResult<Self> {
        bincode::deserialize_from(reader)
            .map_err(|e| IoError::new(IoErrorKind::InvalidData, e.to_string()))
    }

    fn save<O: Write>(&self, writer: O) -> IoResult<()> {
        bincode::serialize_into(writer, self)
            .map_err(|e| IoError::new(IoErrorKind::InvalidData, e.to_string()))
    }
}

#[derive(Serialize, Clone, Debug)]
#[serde_as]
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
}

impl Blockchain {
    /// Creates a new `Blockchain` instance with a database connection.
    pub fn new(db: Db) -> Self {
        Blockchain {
            utxo_set: UtxoSet::new(),
            target: crate::MAX_TARGET,
            db,
            mempool: HashMap::new(),
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

        let mut input_total = Amount::zero();
        for input in &transaction.inputs {
            let prev_output = self
                .find_output_by_outpoint_in_chain_or_utxos(&input.outpoint)?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Input UTXO {}:{} not found for fee calculation",
                        input.outpoint.txid,
                        input.outpoint.vout
                    )
                })?;
            input_total = (input_total + prev_output.value)?;
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
            "Attempting to add block with hash: {} at index {}",
            new_block_hash, new_block.index
        );

        // Get the current tip of our active chain.
        let current_chain_tip = match self
            .get_tip_hash()?
            .and_then(|h| self.get_block_by_hash(&h).transpose())
        {
            Some(Ok(block)) => block,
            None => {
                if new_block.index == 0 && new_block.previous_hash == Hash::zero() {
                    // This is the special case for the genesis block.
                    info!("Chain is empty, processing potential genesis block.");

                    let expected_genesis_hash = crate::utils::genesis_block()?.id()?;
                    if new_block_hash == expected_genesis_hash {
                        self.load_from_db()?;
                        return Ok(AddBlockResult::Added);
                    } else {
                        return Ok(AddBlockResult::Rejected(
                            "Received block is not the correct genesis block.".to_string(),
                        ));
                    }
                }
                return Ok(AddBlockResult::Rejected(
                    "Chain is not initialized. Cannot accept non-genesis peer blocks.".to_string(),
                ));
            }
            Some(Err(e)) => return Err(e).map_err(anyhow::Error::from),
        };

        // Calculate the hash of the current chain tip.
        let current_chain_tip_hash = match current_chain_tip.id() {
            Ok(h) => h,
            Err(e) => {
                error!("Failed to hash current chain tip: {}", e);
                return Ok(AddBlockResult::Rejected(format!(
                    "Failed to hash current chain tip: {}",
                    e
                )));
            }
        };

        // If we already have this block, we don't need to do anything else.
        if self.get_block_by_hash(&new_block_hash)?.is_some() {
            info!(
                "Block {} already exists in the database. Ignoring.",
                new_block_hash
            );
            return Ok(AddBlockResult::Added);
        }

        // Case 1: The new block is a direct extension of our current chain.
        if new_block.previous_hash == current_chain_tip_hash {
            info!(
                "New block {} is a direct extension of the current tip.",
                new_block_hash
            );

            return self.add_direct_extension(new_block, new_block_hash);
        // Case 2: The new block is not a direct extension, indicating a fork or an out-of-order block.
        } else {
            warn!(
                "Fork detected or out-of-order block ({}). Local tip: {} (index {}), New block previous: {} (index {})",
                new_block_hash,
                current_chain_tip_hash, current_chain_tip.index,
                new_block.previous_hash, new_block.index.saturating_sub(1)
            );

            // Before determining the fork type, save the block. This prevents a race condition
            // where the reorg process is triggered but the block it needs is not yet on disk.
            let block_bytes = bincode::serialize(&new_block)?;
            self.db
                .insert(format!("block_{}", new_block_hash).as_bytes(), block_bytes)?;

            // Try to find a common ancestor between our chain and the new block's chain.
            if let Some(common_ancestor_index) =
                self.find_common_ancestor_by_hash(&new_block.previous_hash)
            {
                info!(
                    "Found common ancestor at index {} for received block {}",
                    common_ancestor_index, new_block_hash
                );

                // If the new block's index is not higher, it's a shorter fork and we reject it.
                if new_block.index <= common_ancestor_index {
                    return Ok(AddBlockResult::ShorterForkRejected(format!(
                        "Received block {} is part of a shorter or equal length fork (index {} <= common ancestor index {}). Rejecting.",
                        new_block_hash, new_block.index, common_ancestor_index
                    )));
                }

                // The new block is on a potentially longer fork. Signal this for a reorg.
                Ok(AddBlockResult::PotentialLongerForkDetected {
                    common_ancestor_index,
                    new_block_index: new_block.index,
                    new_block_hash,
                })
            } else {
                // No common ancestor found, the block is an orphan.
                Ok(AddBlockResult::OrphanedOrDisconnected(format!(
                    "Received block {} does not connect to the current chain or any known ancestor. Previous hash: {}",
                    new_block_hash, new_block.previous_hash
                )))
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

        // Pre-validate UTXO application on a temporary copy to ensure it won't fail mid-transaction.
        // This is a pre-check. The actual update happens after the DB transaction succeeds.
        self.utxo_set.clone().apply_block(&new_block)?;

        let tx_count_in_block = new_block.transactions.len() as u64;
        let initial_tx_count = self.get_total_transaction_count_from_db()?;
        let mut total_fees = Amount::zero();
        for tx in &new_block.transactions {
            if !tx.is_coinbase() {
                total_fees = (total_fees + self.calculate_transaction_fee(tx)?)?;
            }
        }
        // Atomically update the database with the new block and associated metadata.
        self.db
            .transaction(
                |tx_db| -> Result<(), ConflictableTransactionError<ReorgError>> {
                    let block_bytes =
                        bincode::serialize(&new_block).map_err(|e| ReorgError::Anyhow(e.into()))?;
                    let hash_bytes = bincode::serialize(&new_block_hash)
                        .map_err(|e| ReorgError::Anyhow(e.into()))?;

                    // Store the block itself, indexed by its hash.
                    tx_db.insert(format!("block_{}", new_block_hash).as_bytes(), block_bytes)?;
                    // Store a mapping from block index to block hash.
                    tx_db.insert(
                        format!("index_{}", new_block.index).as_bytes(),
                        hash_bytes.clone(),
                    )?;

                    // Store transaction locations and chronological order.
                    // Process each transaction in the new block to update various indices.
                    let mut current_tx_index = initial_tx_count;
                    for tx in &new_block.transactions {
                        let tx_hash = tx.txid().map_err(ReorgError::Anyhow)?;
                        let tx_hash_bytes = bincode::serialize(&tx_hash)
                            .map_err(|e| ReorgError::Anyhow(e.into()))?;

                        // Chronological transaction index.
                        // Update history index for all outputs (including coinbase outputs).
                        for output in &tx.outputs {
                            let key = format!("history_{}", output.pubkey.fingerprint());
                            self.add_hash_to_history_list(tx_db, &key, &tx_hash)?;
                        }

                        // For non-coinbase transactions:
                        // - Update history index for inputs (the public key whose UTXO is being spent).
                        // - Store chronological transaction index.
                        // - Increment total transaction count.
                        if !tx.is_coinbase() {
                            for input in &tx.inputs {
                                // Find the original output being spent to get its public key for history indexing.
                                let spent_output = Self::find_output_for_reorg_static(
                                    tx_db,
                                    &input.outpoint,
                                    &[], // No new chain segment when adding a direct extension
                                )
                                .map_err(|e| match e {
                                    sled::transaction::ConflictableTransactionError::Abort(
                                        reorg_err,
                                    ) => reorg_err,
                                    sled::transaction::ConflictableTransactionError::Storage(
                                        err,
                                    ) => ReorgError::Anyhow(err.into()),
                                    _ => ReorgError::Anyhow(anyhow!(
                                        "Unexpected error type during history indexing"
                                    )),
                                })?
                                .ok_or_else(|| {
                                    ReorgError::Anyhow(anyhow!(
                                        "UTXO {} not found for history indexing during block add",
                                        input.outpoint
                                    ))
                                })?;

                                let key = format!("history_{}", spent_output.pubkey.fingerprint());
                                self.add_hash_to_history_list(tx_db, &key, &tx_hash)?;
                            }
                            tx_db.insert(
                                format!("tx_by_order_{}", current_tx_index).as_bytes(),
                                tx_hash_bytes.clone(),
                            )?;
                            current_tx_index += 1;
                        }

                        // Store transaction location (for all transactions, including coinbase).
                        tx_db.insert(
                            format!("tx_location_{}", tx_hash).as_bytes(),
                            &new_block.index.to_be_bytes(),
                        )?;
                    }

                    // Update chain-wide metadata.
                    tx_db.insert(b"tip_hash", hash_bytes)?;
                    tx_db.insert(b"chain_height", &new_block.index.to_be_bytes())?;
                    let new_total_tx_count = initial_tx_count + tx_count_in_block;
                    tx_db.insert(b"total_tx_count", &new_total_tx_count.to_be_bytes())?;

                    // Update total supply.
                    let current_supply_bytes = tx_db.get(b"total_supply")?.unwrap_or_default();
                    let current_supply = u64::from_le_bytes(
                        current_supply_bytes.as_ref().try_into().unwrap_or([0; 8]),
                    );
                    let block_reward = crate::utils::calculate_block_reward(new_block.index);
                    let new_supply = current_supply
                        + block_reward.as_smallest_unit()
                        + total_fees.as_smallest_unit();

                    tx_db.insert(b"total_supply", &new_supply.to_le_bytes())?;

                    Ok(())
                },
            )
            .map_err(|e| match e {
                sled::transaction::TransactionError::Abort(ReorgError::Anyhow(err)) => err,
                sled::transaction::TransactionError::Storage(err) => anyhow::Error::from(err),
            })?;

        info!("Database updated atomically for block {}.", new_block_hash);

        // If the DB transaction was successful, commit the in-memory changes.
        // This is the critical fix: apply the block to the live UTXO set.
        self.utxo_set.apply_block(&new_block)?;

        self.clear_mempool_of_block_transactions(&new_block);
        self.target = expected_next_target;

        // This log provides clear, consistent confirmation when a block is added.
        info!(
            "✅ Block {} (index {}) accepted and added to chain. New height: {}",
            new_block_hash, new_block.index, new_block.index
        );
        Ok(AddBlockResult::Added)
    }

    /// Finds the common ancestor of a potential fork by walking back from the current tip.
    /// It checks if any block in the current chain matches the `previous_hash` of the new block.
    fn find_common_ancestor_by_hash(&self, previous_hash: &Hash) -> Option<u64> {
        let height = self.block_height().ok()?;
        for i in (0..=height).rev() {
            if let Ok(Some(block)) = self.get_block_by_index(i) {
                if let Ok(block_hash) = block.id() {
                    if block_hash == *previous_hash {
                        return Some(i);
                    }
                }
            }
        }
        None
    }
}
