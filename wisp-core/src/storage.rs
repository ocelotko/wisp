use std::collections::HashMap;

use crate::{
    blockchain::{Block, Blockchain},
    currency::Amount,
    mempool::MempoolEntry,
    sha256::Hash,
    signatures::PublicKey, // Keep this for other functions
    transactions::{OutPoint, TransactionOutput},
    utils,
};

use anyhow::{anyhow, Context, Result};
use bincode::config::standard as bincode_config;
use log::{debug, info, warn};
use rayon::prelude::*;
use sled::transaction::TransactionalTree;

/// A centralized definition of all keys used in the Sled database.
/// This prevents typos and serves as documentation for the DB schema.
pub struct DBKeys;

impl DBKeys {
    // --- Singleton Keys ---
    pub const CHAIN_HEIGHT: &'static [u8] = b"chain_height";
    pub const TIP_HASH: &'static [u8] = b"tip_hash";
    pub const TOTAL_TX_COUNT: &'static [u8] = b"total_tx_count";
    pub const TOTAL_SUPPLY: &'static [u8] = b"total_supply";
    pub const UTXO_SNAPSHOT: &'static [u8] = b"utxo_snapshot";
    pub const LAST_UTXO_SNAPSHOT_HEIGHT: &'static [u8] = b"last_utxo_snapshot_height";
    pub const UTXO_SNAPSHOT_CHECKSUM: &'static [u8] = b"utxo_snapshot_checksum";
    pub const MEMPOOL_SNAPSHOT: &'static [u8] = b"mempool_snapshot";
    pub const PENDING_UTXO_SNAPSHOT: &'static [u8] = b"pending_utxo_snapshot";
    pub const PENDING_REORG_TIP: &'static [u8] = b"reorg:tip";
    pub const PENDING_REORG_ANCESTOR: &'static [u8] = b"reorg:ancestor";
    pub const PENDING_REORG_ANCESTOR_HASH: &'static [u8] = b"reorg:ancestor_hash";

    // --- Key Generation Functions for Prefixed Collections ---
    pub fn block(hash: &Hash) -> Vec<u8> {
        format!("block_{}", hash).into_bytes()
    }
    pub fn index_to_hash(index: u64) -> Vec<u8> {
        format!("index_{}", index).into_bytes()
    }
    pub fn hash_to_index(hash: &Hash) -> Vec<u8> {
        format!("hash_to_index_{}", hash).into_bytes()
    }
    pub fn tx_location(txid: &Hash) -> Vec<u8> {
        format!("tx_location_{}", txid).into_bytes()
    }
    pub fn tx_by_order(index: u64) -> Vec<u8> {
        format!("tx_by_order_{}", index).into_bytes()
    }
    pub fn history(pubkey_fingerprint: &str) -> Vec<u8> {
        format!("history_{}", pubkey_fingerprint).into_bytes()
    }
    pub fn prev_to_current(prev_hash: &Hash) -> Vec<u8> {
        format!("prev_to_current_{}", prev_hash).into_bytes()
    }
}

/// Represents the changes a single block makes to the UTXO set.
/// This is used during the parallel phase of the UTXO rebuild.
struct BlockUtxoChanges {
    block_index: u64,
    inputs_to_remove: Vec<OutPoint>,
    outputs_to_add: Vec<(OutPoint, TransactionOutput)>,
}
impl Blockchain {
    /// Loads the blockchain state from the database.
    /// If the database is empty, it initializes it with the genesis block.
    pub fn load_from_db(&mut self) -> Result<()> {
        if !self.db.is_empty() {
            // Check for and handle a crashed reorg before loading the rest of the state.
            if let Err(e) = self.recover_from_crashed_reorg() {
                // If recovery fails, it's safer to halt than to run with a corrupt state.
                log::error!("CRITICAL: Failed to recover from a potential mid-reorg crash. Halting. Error: {}", e);
                return Err(e);
            }

            //TODO: Remove this logic
            // The PENDING_UTXO_SNAPSHOT key is now deprecated, as snapshotting is atomic with block commits.
            // However, we keep this recovery logic for nodes upgrading from a version that might have crashed
            // and left a pending snapshot.
            if self.db.contains_key(DBKeys::PENDING_UTXO_SNAPSHOT)? {
                warn!("Found a deprecated PENDING_UTXO_SNAPSHOT key. This indicates a crash may have occurred on a previous software version. The key will be removed, and a full UTXO rebuild will ensure consistency.");
                self.db.remove(DBKeys::PENDING_UTXO_SNAPSHOT)?;
            }

            // Populate the tip cache
            if let Some(tip_block) = self.get_tip_block()? {
                let tip_hash = tip_block.id()?;
                self.tip_cache = Some((tip_hash, tip_block));
            }

            self.total_supply = Amount::from_smallest_unit(self.get_total_supply_from_db()?);
            self.total_tx_count = self.get_total_transaction_count_from_db()?;

            // If DB is not empty, rebuild the in-memory UTXO set, starting from the last snapshot if available.
            self.rebuild_utxos()?;
            // After rebuilding UTXOs, load and re-validate the mempool.
            if let Err(e) = self.load_mempool_snapshot() {
                warn!(
                    "Could not load mempool from snapshot, starting with an empty one. Error: {}",
                    e
                );
            }

            info!("Database is not empty, loading existing blockchain state.");
            self.target = self.calculate_next_target()?;
            return Ok(());
        }

        // Database is empty, so create and store the genesis block.
        info!("Database is empty. Creating genesis block...");
        let genesis_block = utils::genesis_block()?;
        let genesis_hash = genesis_block.id()?;

        // For the genesis block, we use `add_direct_extension` to ensure all
        // state (DB and in-memory) is initialized consistently, just like any other block.
        // This avoids logic duplication and potential inconsistencies.
        self.add_direct_extension(genesis_block.clone(), genesis_hash)?;

        // Manually populate the tip cache after adding the genesis block.
        self.tip_cache = Some((genesis_hash, genesis_block));

        info!("Genesis Block created and added.");
        info!("Genesis Block Hash: {}", genesis_hash);
        self.save_utxo_snapshot(0)?;
        // And recalculate the target based on the new state.
        self.target = self.calculate_next_target()?;
        self.total_supply = Amount::from_smallest_unit(self.get_total_supply_from_db()?);
        self.total_tx_count = self.get_total_transaction_count_from_db()?;

        Ok(())
    }

    /// Gets the current tip block of the main chain.
    /// It first checks an in-memory cache and falls back to the database if necessary.
    pub fn get_tip_block(&self) -> Result<Option<Block>> {
        if let Some((_, block)) = &self.tip_cache {
            return Ok(Some(block.clone()));
        }

        if let Some(tip_hash) = self.get_tip_hash()? {
            match self.get_block_by_hash(&tip_hash)? {
                Some(block) => Ok(Some(block)),
                None => {
                    let err_msg = format!(
                        "Database inconsistency: Tip hash {} found, but block is missing.",
                        tip_hash
                    );
                    Err(anyhow!(err_msg))
                }
            }
        } else {
            Ok(None)
        }
    }

    /// Retrieves a block from the database by its hash.
    pub fn get_block_by_hash(&self, hash: &Hash) -> Result<Option<Block>> {
        self.db
            .get(DBKeys::block(hash))?
            .map(|ivec| {
                let (checked_block, _): (crate::blockchain::CheckedBlock, _) =
                    bincode::decode_from_slice(&ivec, bincode_config())
                        .context("Failed to deserialize CheckedBlock")?;
                checked_block
                    .into_block()
                    .context("Failed to deserialize block")
            })
            .transpose()
    }

    /// Retrieves a block from the database by its index (height).
    pub fn get_block_by_index(&self, index: u64) -> Result<Option<Block>> {
        if let Some(hash_ivec) = self.db.get(DBKeys::index_to_hash(index))? {
            let (hash, _): (Hash, _) = bincode::decode_from_slice(&hash_ivec, bincode_config())
                .context("Failed to deserialize block hash from index")?;
            self.get_block_by_hash(&hash)
        } else {
            Ok(None)
        }
    }

    /// Retrieves a block from the database by its index within a sled transaction.
    pub fn get_block_by_index_from_db_txn(
        &self,
        index: u64,
        tx_db: &TransactionalTree,
    ) -> Result<Option<Block>> {
        if let Some(hash_ivec) = tx_db.get(DBKeys::index_to_hash(index))? {
            let (hash, _): (Hash, _) = bincode::decode_from_slice(&hash_ivec, bincode_config())?;
            tx_db
                .get(DBKeys::block(&hash))?
                .map(|ivec| {
                    let (checked_block, _): (crate::blockchain::CheckedBlock, _) =
                        bincode::decode_from_slice(&ivec, bincode_config())
                            .context("Failed to deserialize CheckedBlock in txn")?;
                    checked_block
                        .into_block()
                        .context("Failed to deserialize block in txn")
                })
                .transpose()
        } else {
            Ok(None)
        }
    }

    /// A static version of `get_block_by_index_from_db_txn` for use where `&self` isn't available.
    pub fn get_block_by_index_from_db_txn_static(
        index: u64,
        tx_db: &TransactionalTree,
    ) -> Result<Option<Block>> {
        if let Some(hash_ivec) = tx_db.get(DBKeys::index_to_hash(index))? {
            let (hash, _): (Hash, _) = bincode::decode_from_slice(&hash_ivec, bincode_config())?;
            tx_db
                .get(DBKeys::block(&hash))?
                .map(|ivec| {
                    let (checked_block, _): (crate::blockchain::CheckedBlock, _) =
                        bincode::decode_from_slice(&ivec, bincode_config())
                            .context("Failed to deserialize CheckedBlock in txn")?;
                    checked_block
                        .into_block()
                        .context("Failed to deserialize block in txn")
                })
                .transpose()
        } else {
            Ok(None)
        }
    }

    /// Saves a snapshot of the current UTXO set to the database.
    pub fn save_utxo_snapshot(&self, height: u64) -> Result<()> {
        info!("Serializing UTXO set for snapshot at height {}...", height);
        let utxo_bytes = bincode::encode_to_vec(&self.utxo_set, bincode_config())?;
        // Compress the serialized data. The compression level can be adjusted (0 is default).
        let compressed_utxo_bytes =
            zstd::encode_all(&utxo_bytes[..], 0).context("Failed to compress UTXO snapshot")?;
        let compressed_size = compressed_utxo_bytes.len();

        // Calculate a checksum of the compressed data to verify integrity on load.
        let checksum = crate::sha256::hash(&compressed_utxo_bytes[..]);
        let checksum_bytes = bincode::encode_to_vec(&checksum, bincode_config())?;
        let height_bytes = bincode::encode_to_vec(&height, bincode_config())?;

        // Atomically insert the snapshot, its height, and its checksum.
        self.db
            .transaction(|tx_db| {
                tx_db.insert(DBKeys::UTXO_SNAPSHOT, compressed_utxo_bytes.clone())?;
                tx_db.insert(DBKeys::LAST_UTXO_SNAPSHOT_HEIGHT, height_bytes.clone())?;
                tx_db.insert(DBKeys::UTXO_SNAPSHOT_CHECKSUM, checksum_bytes.clone())?;
                Ok(())
            })
            .map_err(|e: sled::transaction::TransactionError| {
                anyhow!("Failed to save snapshot transactionally: {:?}", e)
            })?;

        info!("Saved compressed UTXO snapshot at height {}. Original size: {} bytes, Compressed size: {} bytes, Checksum: {}", height, utxo_bytes.len(), compressed_size, checksum);
        Ok(())
    }

    /// Saves a snapshot of the current mempool to the database.
    pub fn save_mempool_snapshot(&self) -> Result<()> {
        if self.mempool.is_empty() {
            // If the mempool is empty, just remove the key from the DB.
            self.db.remove(DBKeys::MEMPOOL_SNAPSHOT)?;
        } else {
            // Serialize the entire mempool HashMap to preserve timestamps and pre-calculated fees.
            let bytes = bincode::encode_to_vec(&self.mempool, bincode_config())?;
            self.db.insert(DBKeys::MEMPOOL_SNAPSHOT, bytes)?;
        }
        debug!(
            "Saved mempool snapshot with {} transactions.",
            self.mempool.len()
        );
        Ok(())
    }

    /// Loads the mempool from a database snapshot and re-validates each transaction.
    fn load_mempool_snapshot(&mut self) -> Result<()> {
        if let Some(ivec) = self.db.get(DBKeys::MEMPOOL_SNAPSHOT)? {
            // Decode the full HashMap<Hash, MempoolEntry>
            let (snapshot_mempool, _): (HashMap<Hash, MempoolEntry>, _) =
                bincode::decode_from_slice(&ivec, bincode_config())
                    .context("Failed to decode mempool snapshot")?;

            info!(
                "Loading {} transactions from mempool snapshot...",
                snapshot_mempool.len()
            );
            let mut successfully_loaded = 0;
            for (tx_hash, entry) in snapshot_mempool {
                // Re-validate each transaction against the current state.
                // This is a crucial safety check.
                // We can skip the full `add_to_mempool` which recalculates fees,
                // and do a quicker validation before inserting directly.
                // For simplicity here, we'll just re-add, but a more optimized path is possible.
                if self.add_to_mempool(entry.transaction.clone()).is_ok() {
                    successfully_loaded += 1;
                } else {
                    warn!("Could not re-validate transaction {} from mempool snapshot. It may now be invalid.", tx_hash);
                }
            }
            info!(
                "Successfully re-validated and loaded {} transactions into the mempool.",
                successfully_loaded
            );
        }
        Ok(())
    }

    /// Gets the height at which the last UTXO snapshot was saved.
    fn get_last_utxo_snapshot_height(&self) -> Result<Option<u64>> {
        self.db
            .get(DBKeys::LAST_UTXO_SNAPSHOT_HEIGHT)?
            .map(|ivec| -> Result<u64> {
                bincode::decode_from_slice(&ivec, bincode_config())
                    .map(|(h, _)| h)
                    .context("Failed to deserialize last_utxo_snapshot_height with bincode")
            })
            .transpose() // Option<Result<T>> -> Result<Option<T>>
    }
    /// Gets the current height of the blockchain from the database.
    /// It prioritizes the in-memory tip cache for immediate consistency.
    pub fn block_height(&self) -> Result<u64> {
        if let Some((_, tip_block)) = &self.tip_cache {
            return Ok(tip_block.index);
        }
        if let Some(ivec) = self.db.get(DBKeys::CHAIN_HEIGHT)? {
            // Handle raw 8-byte big-endian format first, which is the canonical format.
            if ivec.len() == 8 {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&ivec);
                Ok(u64::from_be_bytes(arr))
            } else {
                // Fallback for backward compatibility with old bincode-encoded values.
                bincode::decode_from_slice(&ivec, bincode_config())
                    .map(|(v, _)| v)
                    .with_context(|| {
                        format!(
                            "Invalid DB entry for CHAIN_HEIGHT: expected 8 bytes, found {}",
                            ivec.len()
                        )
                    })
            }
        } else {
            // If the key doesn't exist, the height is 0 (e.g., before genesis).
            Ok(0)
        }
    }

    /// Gets the hash of the current chain tip from the database.
    pub fn get_tip_hash(&self) -> Result<Option<Hash>> {
        // Check the in-memory cache first.
        if let Some((hash, _)) = self.tip_cache {
            return Ok(Some(hash));
        }

        self.db
            .get(DBKeys::TIP_HASH)?
            .map(|ivec| {
                bincode::decode_from_slice(&ivec, bincode_config())
                    .map(|(h, _)| h)
                    .context("Failed to deserialize tip hash with bincode")
            })
            .transpose() // Option<Result<T>> -> Result<Option<T>>
    }

    /// Gets the total number of confirmed (non-coinbase) transactions from the database.
    pub fn get_total_transaction_count_from_db(&self) -> Result<u64> {
        if let Some(ivec) = self.db.get(DBKeys::TOTAL_TX_COUNT)? {
            // Handle raw 8-byte big-endian format first.
            if ivec.len() == 8 {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&ivec);
                Ok(u64::from_be_bytes(arr))
            } else {
                // Fallback for backward compatibility.
                bincode::decode_from_slice(&ivec, bincode_config())
                    .map(|(v, _)| v)
                    .with_context(|| {
                        format!(
                            "Invalid DB entry for TOTAL_TX_COUNT: expected 8 bytes, found {}",
                            ivec.len()
                        )
                    })
            }
        } else {
            Ok(0)
        }
    }

    /// Gets the total circulating supply (in smallest units) from the database.
    pub fn get_total_supply_from_db(&self) -> Result<u64> {
        if let Some(ivec) = self.db.get(DBKeys::TOTAL_SUPPLY)? {
            // Handle raw 8-byte big-endian format first.
            if ivec.len() == 8 {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&ivec);
                Ok(u64::from_be_bytes(arr))
            } else {
                // Fallback for backward compatibility.
                bincode::decode_from_slice(&ivec, bincode_config())
                    .map(|(v, _)| v)
                    .with_context(|| {
                        format!(
                            "Invalid DB entry for TOTAL_SUPPLY: expected 8 bytes, found {}",
                            ivec.len()
                        )
                    })
            }
        } else {
            Ok(0)
        }
    }

    /// Sets the total circulating supply in the database.
    pub fn set_total_supply(&self, count: u64) -> Result<()> {
        self.db
            .insert(DBKeys::TOTAL_SUPPLY, count.to_be_bytes().to_vec())?;
        Ok(())
    }

    /// Gets a transaction hash by its chronological order index.
    pub fn get_transaction_hash_by_chronological_index(&self, index: u64) -> Result<Option<Hash>> {
        self.db
            .get(DBKeys::tx_by_order(index))?
            .map(|ivec| {
                // This function seems unused, but let's fix it anyway.
                bincode::decode_from_slice(&ivec, bincode_config())
                    .map(|(h, _)| h)
                    .context("Failed to deserialize tx hash from chronological index with bincode")
            })
            .transpose()
    }

    /// Gets all transaction hashes associated with a public key from the `history_` index.
    pub fn get_transaction_hashes_by_pubkey_from_db(
        &self,
        pubkey: &PublicKey,
    ) -> Result<Vec<Hash>> {
        let result = self
            .db
            .get(DBKeys::history(&pubkey.fingerprint()))?
            .map(|ivec| {
                bincode::decode_from_slice(&ivec, bincode_config())
                    .map(|(v, _)| v)
                    .context("Failed to deserialize history list")
            })
            .transpose()?
            .unwrap_or_default();
        Ok(result)
    }

    /// Reconstructs the in-memory UTXO set by iterating through all blocks in the database from genesis.
    /// This is a crucial step on node startup to ensure the in-memory state is consistent with the on-disk state.
    pub fn rebuild_utxos(&mut self) -> Result<()> {
        // Try to load from a snapshot first.
        let (mut new_utxos, start_height) = if let (Some(snapshot_ivec), Some(checksum_ivec)) = (
            self.db.get(DBKeys::UTXO_SNAPSHOT)?,
            self.db.get(DBKeys::UTXO_SNAPSHOT_CHECKSUM)?,
        ) {
            let (expected_checksum, _): (Hash, _) =
                bincode::decode_from_slice(&checksum_ivec, bincode_config())
                    .context("Failed to decode UTXO snapshot checksum")?;

            let actual_checksum = crate::sha256::hash(&snapshot_ivec[..]);

            if actual_checksum == expected_checksum {
                info!("UTXO snapshot checksum matches. Loading from snapshot.");
                let decompressed_bytes = zstd::decode_all(&snapshot_ivec[..])
                    .context("Failed to decompress UTXO snapshot")?;
                let (snapshot, _): (crate::utxo::UtxoSet, _) =
                    bincode::decode_from_slice(&decompressed_bytes, bincode_config())
                        .context("Failed to deserialize UTXO snapshot after decompression")?;
                let snapshot_height = self.get_last_utxo_snapshot_height()?.unwrap_or(0);
                (snapshot.utxos, snapshot_height + 1)
            } else {
                warn!(
                        "UTXO snapshot checksum mismatch! Expected: {}, Actual: {}. Discarding snapshot and rebuilding from genesis.",
                        expected_checksum, actual_checksum
                    );
                // Checksum failed, discard the snapshot and start from scratch.
                (std::collections::HashMap::new(), 0)
            }
        } else {
            // If no snapshot or checksum is found, start from genesis.
            if self.db.get(DBKeys::UTXO_SNAPSHOT)?.is_some() {
                warn!("Found a UTXO snapshot but no checksum. Discarding snapshot and rebuilding from genesis.");
            } else {
                info!("No UTXO snapshot found. Rebuilding from genesis.");
            }
            (std::collections::HashMap::new(), 0)
        };

        let height = self.block_height()?;
        if start_height > height + 1 {
            return Err(anyhow!(
                "UTXO snapshot height ({}) is greater than current chain height ({}). DB may be corrupt.",
                start_height - 1,
                height
            ));
        }

        if height >= start_height {
            info!(
                "Rebuilding UTXOs from block {} to {}...",
                start_height, height
            );

            // Process blocks in chunks to avoid loading the entire chain into memory.
            const CHUNK_SIZE: u64 = 1000;
            let mut i = start_height;
            while i <= height {
                let end = std::cmp::min(i + CHUNK_SIZE - 1, height);
                info!("Processing block chunk {} to {}...", i, end);

                // 1. Collect a chunk of blocks to separate I/O from CPU-bound work.
                let blocks_to_process: Vec<Block> = (i..=end)
                    .map(|idx| self.get_block_by_index(idx))
                    .collect::<Result<Vec<Option<Block>>>>()?
                    .into_iter()
                    .flatten()
                    .collect();

                // 2. Map Phase (Parallel): Process blocks in the chunk to compute UTXO changes.
                // This logic remains the same, but now operates on a smaller `blocks_to_process` Vec.
                let mut block_changes: Vec<BlockUtxoChanges> = blocks_to_process
                    .into_par_iter()
                    .map(|block| {
                        let mut inputs_to_remove = Vec::new();
                        let mut outputs_to_add = Vec::new();

                        for tx in &block.transactions {
                            if !tx.is_coinbase() {
                                for input in &tx.inputs {
                                    inputs_to_remove.push(input.outpoint);
                                }
                            }

                            // This can fail if hashing fails, but it's highly unlikely for valid block data.
                            if let Ok(txid) = tx.txid() {
                                for (vout, output) in tx.outputs.iter().enumerate() {
                                    let outpoint = OutPoint {
                                        txid,
                                        vout: vout as u32,
                                    };
                                    outputs_to_add.push((outpoint, output.clone()));
                                }
                            }
                        }

                        BlockUtxoChanges {
                            block_index: block.index,
                            inputs_to_remove,
                            outputs_to_add,
                        }
                    })
                    .collect();

                // Sort the changes by block index to ensure sequential application.
                block_changes.sort_by_key(|c| c.block_index);

                // 3. Reduce Phase (Sequential): Apply the collected changes for the chunk.
                for changes in block_changes {
                    for outpoint in changes.inputs_to_remove {
                        new_utxos.remove(&outpoint);
                    }
                    for (outpoint, output) in changes.outputs_to_add {
                        new_utxos.insert(outpoint, output);
                    }
                }
                i = end + 1;
            }
        }
        info!("UTXO rebuild complete. Finalizing state...");

        // After rebuilding the UTXO set, iterate through the mempool and mark spent UTXOs.
        // This ensures the in-memory UTXO state is consistent with pending transactions.
        for entry in self.mempool.values() {
            for input in &entry.transaction.inputs {
                if new_utxos.contains_key(&input.outpoint) {
                    self.mempool_spent_utxos.insert(input.outpoint); // Mark as spent in mempool
                }
            }
        }

        // Replace the old in-memory UTXO set with the newly built one.
        self.utxo_set.utxos = new_utxos;

        // Save a final snapshot at the current tip, but throttle it to avoid
        // expensive re-compression on every startup for large chains.
        const REBUILD_SNAPSHOT_INTERVAL: u64 = 10_000;
        if height > 0 && height % REBUILD_SNAPSHOT_INTERVAL == 0 {
            self.save_utxo_snapshot(height)?;
        }
        Ok(())
    }

    /// Checks for and attempts to recover from a failed reorganization that was interrupted.
    fn recover_from_crashed_reorg(&mut self) -> Result<()> {
        let pending_tip_ivec = self.db.get(DBKeys::PENDING_REORG_TIP)?;
        let pending_ancestor_ivec = self.db.get(DBKeys::PENDING_REORG_ANCESTOR)?;

        if let (Some(tip_ivec), Some(ancestor_ivec)) = (pending_tip_ivec, pending_ancestor_ivec) {
            let (pending_tip_hash, _): (Hash, _) =
                bincode::decode_from_slice(&tip_ivec, bincode_config())
                    .context("Failed to decode pending reorg tip hash")?;
            let (common_ancestor_index, _): (u64, _) =
                bincode::decode_from_slice(&ancestor_ivec, bincode_config())
                    .context("Failed to decode pending reorg ancestor index")?;

            warn!(
                "Node appears to have crashed during a previous reorg. Attempting recovery. New tip: {}, Ancestor index: {}",
                pending_tip_hash, common_ancestor_index
            );

            // Reconstruct the new chain segment by walking backwards from the pending tip.
            let mut new_chain_segment = Vec::new();
            let mut current_hash = pending_tip_hash;

            loop {
                let block = self.get_block_by_hash(&current_hash)?.ok_or_else(|| {
                    anyhow!(
                        "Recovery failed: Could not find block {} from pending reorg chain.",
                        current_hash
                    )
                })?;

                // Stop when we reach the block just after the common ancestor.
                if block.index <= common_ancestor_index {
                    break;
                }

                new_chain_segment.push(block.clone());
                current_hash = block.previous_hash;

                // Safety break if we walk back too far without finding the ancestor.
                if current_hash == Hash::zero() && common_ancestor_index > 0 {
                    return Err(anyhow!(
                        "Recovery failed: Fork chain does not connect to known ancestor."
                    ));
                }
            }

            // The blocks were pushed in reverse order, so we need to reverse the list.
            new_chain_segment.reverse();

            info!(
                "Re-initiating reorganization with a recovered chain segment of length {}.",
                new_chain_segment.len()
            );
            self.reorganize_chain(new_chain_segment, common_ancestor_index)?;

            info!("✅ Successfully recovered from previous reorg attempt.");
        }

        Ok(())
    }
}
