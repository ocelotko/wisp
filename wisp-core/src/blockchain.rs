use crate::{
    currency::Amount,
    reorg::ReorgError,
    sha256::{hash, Hash, Hashable, Sha256},
    transactions::{OutPoint, Transaction, TransactionOutput},
    utils::MerkleRoot,
    utxo::UtxoSet,
    U256,
};
use anyhow::anyhow;
use anyhow::Result;
use bincode::config::standard as bincode_config;
use bincode::{Decode, Encode};
use chrono::{DateTime, Utc};
use log::{error, info, warn};
use serde::Deserialize;
use serde::Serialize;
use serde_with::serde_as;
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
    /// The block's previous hash does not correspond to any known block in the chain, making it an orphan.
    OrphanedOrDisconnected(String),
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

#[derive(Encode, Decode, Serialize)]
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

        // Get the current tip of our active chain. If it doesn't exist, we're on an empty chain.
        let maybe_current_tip = match self
            .get_tip_hash()?
            .and_then(|h| self.get_block_by_hash(&h).transpose())
        {
            Some(Ok(block)) => block,
            None => {
                // Chain is empty. We must be processing the genesis block.
                if new_block.index == 0 && new_block.previous_hash == Hash::zero() {
                    info!("Chain is empty, processing genesis block.");
                    return self.add_direct_extension(new_block, new_block_hash);
                }
                // If the chain is empty and we get a non-genesis block, it's an error.
                return Ok(AddBlockResult::Rejected(
                    "Chain is not initialized. Cannot accept non-genesis peer blocks.".to_string(),
                ));
            }
            Some(Err(e)) => return Err(e).map_err(anyhow::Error::from),
        };

        let current_chain_tip = maybe_current_tip;
        let current_chain_tip_hash = current_chain_tip.id()?;

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
            let block_bytes = bincode::encode_to_vec(&new_block, bincode_config())?;
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

                // If the new block's index is not higher than our current tip, it can't be a longer chain.
                if new_block.index <= current_chain_tip.index {
                    return Ok(AddBlockResult::ShorterForkRejected(format!(
                        "Received block {} is part of a shorter or equal length fork (new index {} <= current index {}). Rejecting.",
                        new_block_hash, new_block.index, current_chain_tip.index
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
        info!(
            "🩵 Starting add_direct_extension for block index={} hash={}",
            new_block.index, new_block_hash
        );
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
                    let block_bytes = bincode::encode_to_vec(&new_block, bincode_config())
                        .map_err(|e| ReorgError::Anyhow(e.into()))?;
                    let hash_bytes = bincode::encode_to_vec(&new_block_hash, bincode_config())
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
                        let tx_hash_bytes = bincode::encode_to_vec(&tx_hash, bincode_config())
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

        // If the DB transaction was successful, commit the in-memory changes:
        // 1. Apply the block to the live UTXO set.
        self.utxo_set.apply_block(&new_block)?;
        // 2. Remove transactions from the mempool that were included in the block.
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
        blockchain: &Blockchain,
        prev_block: &Block,
        transactions: Vec<Transaction>,
    ) -> Block {
        let index = prev_block.index + 1;
        let previous_hash = prev_block.id().unwrap();
        // Ensure the timestamp is always greater than the previous block's to pass MTP validation.
        let timestamp = prev_block.timestamp + chrono::Duration::seconds(1);

        // A block must always have a coinbase transaction.
        // We create a dummy one here for testing purposes.
        let mut block_transactions = transactions;
        let coinbase_output = TransactionOutput {
            // The reward must be correct for the block to be valid.
            value: utils::calculate_block_reward(index),
            pubkey: utils::genesis_block().unwrap().transactions[0].outputs[0] // Use a known pubkey
                .pubkey
                .clone(), // Use a known pubkey
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

        // Use the blockchain's calculated next target to ensure the block is valid.
        let target = blockchain.calculate_next_target().unwrap();
        let mut new_block = Block::new(
            1,
            timestamp,
            0, // nonce starts at 0
            previous_hash,
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

        let second_block = create_next_block(&blockchain, &genesis_block, vec![]);
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

        let mut bad_block = create_next_block(&blockchain, &genesis_block, vec![]);
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
        let main_chain_block_1 = create_next_block(&blockchain, &genesis_block, vec![]);
        blockchain.add_block(main_chain_block_1.clone()).unwrap();
        assert_eq!(blockchain.block_height().unwrap(), 1);

        // A competing block is mined, also based on genesis. This creates a fork of equal length.
        let fork_block_1 = create_next_block(&blockchain, &genesis_block, vec![]);

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
        let main_chain_block_1 = create_next_block(&blockchain, &genesis_block, vec![]);
        blockchain.add_block(main_chain_block_1.clone()).unwrap();
        assert_eq!(blockchain.block_height().unwrap(), 1);

        // Now, a fork appears that is longer.
        // Fork Block 1 (builds on genesis)
        let fork_block_1 = create_next_block(&blockchain, &genesis_block, vec![]);
        // Fork Block 2 (builds on Fork Block 1)
        let fork_block_2 = create_next_block(&blockchain, &fork_block_1, vec![]);

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
        let main_chain_block_1 = create_next_block(&blockchain, &genesis_block, vec![]);
        blockchain.add_block(main_chain_block_1.clone()).unwrap();
        assert_eq!(blockchain.block_height().unwrap(), 1);
        assert_eq!(
            blockchain.get_tip_hash().unwrap().unwrap(),
            main_chain_block_1.id().unwrap()
        );

        // 2. Create a longer competing fork of length 2 (total height 2)
        let fork_block_1 = create_next_block(&blockchain, &genesis_block, vec![]);
        let fork_block_2 = create_next_block(&blockchain, &fork_block_1, vec![]);

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
            2,
            "Chain height should be updated after reorg"
        );
        assert_eq!(
            blockchain.get_tip_hash().unwrap().unwrap(),
            fork_block_2.id().unwrap(),
            "Chain tip should be the new fork's tip after reorg"
        );
    }
}
