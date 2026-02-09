use std::collections::HashMap;

use crate::{
    blockchain::{Block, BlockHeader, Blockchain},
    currency::Amount,
    mempool::MempoolEntry,
    sha256::Hash,
    signatures::PublicKey,
    transactions::{OutPoint, TransactionOutput},
    utils,
};

use anyhow::{anyhow, Context, Result};
use bincode::config::standard as bincode_config;
use log::{debug, error, info, warn};
use rayon::prelude::*;
use sled::transaction::TransactionalTree;

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

struct BlockUtxoChanges {
    block_index: u64,
    inputs_to_remove: Vec<OutPoint>,
    outputs_to_add: Vec<(OutPoint, TransactionOutput)>,
}
impl Blockchain {
    pub fn load_from_db(&mut self) -> Result<()> {
        if !self.db.is_empty() {
            if let Err(e) = self.recover_from_crashed_reorg() {
                log::error!("CRITICAL: Failed to recover from a potential mid-reorg crash. Halting. Error: {}", e);
                return Err(e);
            }

            if let Some(tip_block) = self.get_tip_block()? {
                let tip_hash = tip_block.id()?;
                self.tip_cache = Some((tip_hash, tip_block));
            }

            self.total_supply = Amount::from_smallest_unit(self.get_total_supply_from_db()?);
            self.total_tx_count = self.get_total_transaction_count_from_db()?;

            if let Err(e) = self.verify_supply_integrity() {
                error!(
                    "CRITICAL: Database corrupted. Supply check failed on load: {}",
                    e
                );
                return Err(e);
            }

            self.rebuild_utxos()?;
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

        info!("Database is empty. Creating genesis block...");
        let genesis_block = utils::genesis_block()?;
        let genesis_hash = genesis_block.id()?;

        self.add_direct_extension(genesis_block.clone(), genesis_hash)?;
        self.tip_cache = Some((genesis_hash, genesis_block));

        info!("Genesis Block created and added.");
        info!("Genesis Block Hash: {}", genesis_hash);

        self.save_utxo_snapshot(0)?;
        self.target = self.calculate_next_target()?;
        self.total_supply = Amount::from_smallest_unit(self.get_total_supply_from_db()?);
        self.total_tx_count = self.get_total_transaction_count_from_db()?;

        Ok(())
    }

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

    pub fn get_block_header_by_hash(&self, hash: &Hash) -> Result<Option<BlockHeader>> {
        self.db
            .get(DBKeys::block(hash))?
            .map(|ivec| {
                // Optimization: CheckedBlock starts with Block, which starts with BlockHeader.
                // We can decode just the header to avoid parsing transactions.
                let (header, _): (BlockHeader, _) =
                    bincode::decode_from_slice(&ivec, bincode_config())
                        .context("Failed to deserialize BlockHeader")?;
                Ok(header)
            })
            .transpose()
    }

    pub fn get_block_by_index(&self, index: u64) -> Result<Option<Block>> {
        if let Some(hash_ivec) = self.db.get(DBKeys::index_to_hash(index))? {
            let (hash, _): (Hash, _) = bincode::decode_from_slice(&hash_ivec, bincode_config())
                .context("Failed to deserialize block hash from index")?;
            self.get_block_by_hash(&hash)
        } else {
            Ok(None)
        }
    }

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

    pub fn save_utxo_snapshot(&self, height: u64) -> Result<()> {
        info!("Serializing UTXO set for snapshot at height {}...", height);
        let utxo_bytes = bincode::encode_to_vec(&self.utxo_set, bincode_config())?;
        let compressed_utxo_bytes =
            zstd::encode_all(&utxo_bytes[..], 0).context("Failed to compress UTXO snapshot")?;
        let compressed_size = compressed_utxo_bytes.len();

        let checksum = crate::sha256::hash(&compressed_utxo_bytes[..]);
        let checksum_bytes = bincode::encode_to_vec(&checksum, bincode_config())?;
        let height_bytes = bincode::encode_to_vec(&height, bincode_config())?;

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

    pub fn save_mempool_snapshot(&self) -> Result<()> {
        if self.mempool.is_empty() {
            self.db.remove(DBKeys::MEMPOOL_SNAPSHOT)?;
        } else {
            let bytes = bincode::encode_to_vec(&self.mempool, bincode_config())?;
            self.db.insert(DBKeys::MEMPOOL_SNAPSHOT, bytes)?;
        }
        debug!(
            "Saved mempool snapshot with {} transactions.",
            self.mempool.len()
        );
        Ok(())
    }

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
                if self
                    .add_transaction_to_mempool(entry.transaction.clone(), false)
                    .is_ok()
                {
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

    fn get_last_utxo_snapshot_height(&self) -> Result<Option<u64>> {
        self.db
            .get(DBKeys::LAST_UTXO_SNAPSHOT_HEIGHT)?
            .map(|ivec| -> Result<u64> {
                bincode::decode_from_slice(&ivec, bincode_config())
                    .map(|(h, _)| h)
                    .context("Failed to deserialize last_utxo_snapshot_height with bincode")
            })
            .transpose()
    }

    pub fn block_height(&self) -> Result<u64> {
        if let Some((_, tip_block)) = &self.tip_cache {
            return Ok(tip_block.index);
        }
        if let Some(ivec) = self.db.get(DBKeys::CHAIN_HEIGHT)? {
            if ivec.len() == 8 {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&ivec);
                Ok(u64::from_be_bytes(arr))
            } else {
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
            Ok(0)
        }
    }

    pub fn get_tip_hash(&self) -> Result<Option<Hash>> {
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
            .transpose()
    }

    pub fn get_total_transaction_count_from_db(&self) -> Result<u64> {
        if let Some(ivec) = self.db.get(DBKeys::TOTAL_TX_COUNT)? {
            if ivec.len() == 8 {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&ivec);
                Ok(u64::from_be_bytes(arr))
            } else {
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

    pub fn get_total_supply_from_db(&self) -> Result<u64> {
        if let Some(ivec) = self.db.get(DBKeys::TOTAL_SUPPLY)? {
            if ivec.len() == 8 {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&ivec);
                Ok(u64::from_be_bytes(arr))
            } else {
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

    pub fn set_total_supply(&self, count: u64) -> Result<()> {
        self.db
            .insert(DBKeys::TOTAL_SUPPLY, count.to_be_bytes().to_vec())?;
        Ok(())
    }

    pub fn get_transaction_hash_by_chronological_index(&self, index: u64) -> Result<Option<Hash>> {
        self.db
            .get(DBKeys::tx_by_order(index))?
            .map(|ivec| {
                bincode::decode_from_slice(&ivec, bincode_config())
                    .map(|(h, _)| h)
                    .context("Failed to deserialize tx hash from chronological index with bincode")
            })
            .transpose()
    }

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

    pub fn rebuild_utxos(&mut self) -> Result<()> {
        // Check if a valid snapshot exists
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
                (std::collections::HashMap::new(), 0)
            }
        } else {
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

            const CHUNK_SIZE: u64 = 1000;
            let mut i = start_height;
            while i <= height {
                let end = std::cmp::min(i + CHUNK_SIZE - 1, height);
                info!("Processing block chunk {} to {}...", i, end);

                let blocks_to_process: Vec<Block> = (i..=end)
                    .map(|idx| self.get_block_by_index(idx))
                    .collect::<Result<Vec<Option<Block>>>>()?
                    .into_iter()
                    .flatten()
                    .collect();

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

                block_changes.sort_by_key(|c| c.block_index);

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

        for entry in self.mempool.values() {
            for input in &entry.transaction.inputs {
                if new_utxos.contains_key(&input.outpoint) {
                    self.mempool_spent_utxos.insert(input.outpoint);
                }
            }
        }

        self.utxo_set.utxos = new_utxos;

        const REBUILD_SNAPSHOT_INTERVAL: u64 = 10_000;
        if height > 0 && height % REBUILD_SNAPSHOT_INTERVAL == 0 {
            self.save_utxo_snapshot(height)?;
        }
        Ok(())
    }

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

            let mut new_chain_segment = Vec::new();
            let mut current_hash = pending_tip_hash;

            loop {
                let block = self.get_block_by_hash(&current_hash)?.ok_or_else(|| {
                    anyhow!(
                        "Recovery failed: Could not find block {} from pending reorg chain.",
                        current_hash
                    )
                })?;

                if block.index <= common_ancestor_index {
                    break;
                }

                new_chain_segment.push(block.clone());
                current_hash = block.header.previous_hash;

                if current_hash == Hash::zero() && common_ancestor_index > 0 {
                    return Err(anyhow!(
                        "Recovery failed: Fork chain does not connect to known ancestor."
                    ));
                }
            }

            new_chain_segment.reverse();

            info!(
                "Re-initiating reorganization with a recovered chain segment of length {}.",
                new_chain_segment.len()
            );
            self.reorganize_chain(new_chain_segment, common_ancestor_index)?;

            info!("Successfully recovered from previous reorg attempt.");
        }

        Ok(())
    }

    pub fn migrate_total_supply(&mut self) -> Result<()> {
        if self.db.is_empty() {
            info!("Database is empty, skipping migration.");
            return Ok(());
        }

        let height = self.block_height()?;
        let expected_supply = crate::utils::calculate_expected_supply(height);
        let current_db_supply = self.get_total_supply_from_db()?;

        if current_db_supply != expected_supply.as_smallest_unit() {
            warn!(
                "Migrating Total Supply: Current DB value ({}) does not match expected value ({}). Updating...",
                current_db_supply, expected_supply
            );
            self.set_total_supply(expected_supply.as_smallest_unit())?;
            self.total_supply = expected_supply;
            info!("Total Supply migration completed successfully.");
        } else {
            info!("Total Supply is already correct. No migration needed.");
        }

        Ok(())
    }
}
