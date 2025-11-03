use crate::{
    blockchain::{Block, Blockchain},
    sha256::Hash,
    signatures::PublicKey,
    transactions::OutPoint,
    utils,
};

use anyhow::{Context, Result};
use bincode::config::standard as bincode_config;
use log::info;
impl Blockchain {
    const UTXO_SNAPSHOT_INTERVAL: u64 = 720; // Save a snapshot every 720 blocks

    /// Loads the blockchain state from the database.
    /// If the database is empty, it initializes it with the genesis block.
    pub fn load_from_db(&mut self) -> Result<()> {
        if !self.db.is_empty() {
            // If DB is not empty, rebuild the in-memory UTXO set from the last snapshot.
            let last_snapshot_height = self.get_last_utxo_snapshot_height()?.unwrap_or(0);
            info!(
                "Rebuilding UTXO set from last snapshot at height: {}",
                last_snapshot_height
            );
            self.rebuild_utxos()?;
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
        self.add_direct_extension(genesis_block, genesis_hash)?;

        info!("Genesis Block created and added.");
        info!("Genesis Block Hash: {}", genesis_hash);
        self.save_utxo_snapshot(0)?;
        // And recalculate the target based on the new state.
        self.target = self.calculate_next_target()?;

        Ok(())
    }

    /// Retrieves a block from the database by its hash.
    pub fn get_block_by_hash(&self, hash: &Hash) -> Result<Option<Block>> {
        let key = format!("block_{}", hash);
        self.db
            .get(key)?
            .map(|ivec| {
                bincode::decode_from_slice(&ivec, bincode_config())
                    .map(|(b, _)| b)
                    .context("Failed to deserialize block")
            })
            .transpose()
    }

    /// Retrieves a block from the database by its index (height).
    pub fn get_block_by_index(&self, index: u64) -> Result<Option<Block>> {
        let hash_key = format!("index_{}", index);
        if let Some(hash_ivec) = self.db.get(hash_key)? {
            let (hash, _): (Hash, _) = bincode::decode_from_slice(&hash_ivec, bincode_config())?;
            self.get_block_by_hash(&hash)
        } else {
            Ok(None)
        }
    }

    /// Saves a snapshot of the current UTXO set to the database.
    pub fn save_utxo_snapshot(&self, height: u64) -> Result<()> {
        let utxo_bytes = bincode::encode_to_vec(&self.utxo_set, bincode_config())?;
        self.db.insert("utxo_snapshot", utxo_bytes)?;
        self.db
            .insert("last_utxo_snapshot_height", &height.to_be_bytes())?;
        info!("Saved UTXO snapshot at height {}", height);
        Ok(())
    }

    /// Gets the height at which the last UTXO snapshot was saved.
    fn get_last_utxo_snapshot_height(&self) -> Result<Option<u64>> {
        self.db
            .get("last_utxo_snapshot_height")?
            .map(|ivec| {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&ivec);
                Ok(u64::from_be_bytes(bytes))
            })
            .transpose()
    }
    /// Gets the current height of the blockchain from the database.
    pub fn block_height(&self) -> Result<u64> {
        self.db
            .get("chain_height")?
            .map(|ivec| {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&ivec);
                Ok(u64::from_be_bytes(bytes))
            })
            .unwrap_or(Ok(0))
    }

    /// Sets the current height of the blockchain in the database.
    pub fn set_block_height(&self, height: u64) -> Result<()> {
        self.db.insert("chain_height", &height.to_be_bytes())?;
        Ok(())
    }

    /// Gets the hash of the current chain tip from the database.
    pub fn get_tip_hash(&self) -> Result<Option<Hash>> {
        self.db
            .get("tip_hash")?
            .map(|ivec| {
                bincode::decode_from_slice(&ivec, bincode_config())
                    .map(|(h, _)| h)
                    .context("Failed to deserialize tip hash with bincode")
            })
            .transpose()
    }

    /// Sets the hash of the current chain tip in the database.
    pub fn set_tip_hash(&self, hash: &Hash) -> Result<()> {
        let bytes = bincode::encode_to_vec(hash, bincode_config())?;
        self.db.insert("tip_hash", bytes)?;
        Ok(())
    }

    /// Gets the total number of confirmed (non-coinbase) transactions from the database.
    pub fn get_total_transaction_count_from_db(&self) -> Result<u64> {
        self.db
            .get("total_tx_count")?
            .map(|ivec| {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&ivec);
                Ok(u64::from_be_bytes(bytes))
            })
            .unwrap_or(Ok(0))
    }

    /// Sets the total number of confirmed transactions in the database.
    pub fn set_total_transaction_count(&self, count: u64) -> Result<()> {
        self.db.insert("total_tx_count", &count.to_be_bytes())?;
        Ok(())
    }

    /// Gets the total circulating supply (in smallest units) from the database.
    pub fn get_total_supply_from_db(&self) -> Result<u64> {
        self.db
            .get("total_supply")?
            .map(|ivec| {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&ivec);
                Ok(u64::from_be_bytes(bytes))
            })
            .unwrap_or(Ok(0))
    }

    /// Sets the total circulating supply in the database.
    pub fn set_total_supply(&self, count: u64) -> Result<()> {
        self.db.insert("total_supply", &count.to_be_bytes())?;
        Ok(())
    }

    /// (Legacy/Unused) Gets a transaction hash by its outpoint.
    pub fn get_transaction_hash_by_outpoint_from_db(
        &self,
        outpoint: &OutPoint,
    ) -> Result<Option<Hash>> {
        let key = format!("tx_by_outpoint_{}", outpoint);
        self.db
            .get(key)?
            .map(|ivec| {
                // This function seems unused, but let's fix it anyway.
                bincode::decode_from_slice(&ivec, bincode_config())
                    .map(|(h, _)| h)
                    .context("Failed to deserialize tx hash from outpoint index with bincode")
            })
            .transpose()
    }

    /// Gets a transaction hash by its chronological order index.
    pub fn get_transaction_hash_by_chronological_index(&self, index: u64) -> Result<Option<Hash>> {
        let key = format!("tx_by_order_{}", index);
        self.db
            .get(key)?
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
        let key = format!("history_{}", pubkey.fingerprint());
        let result = self
            .db
            .get(key)?
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
        let (mut new_utxos, start_height) =
            if let Some(snapshot_ivec) = self.db.get("utxo_snapshot")? {
                let (snapshot, _): (crate::utxo::UtxoSet, _) =
                    bincode::decode_from_slice(&snapshot_ivec, bincode_config())?;
                let snapshot_height = self.get_last_utxo_snapshot_height()?.unwrap_or(0);
                (snapshot.utxos, snapshot_height + 1)
            } else {
                // If no snapshot, start from genesis.
                (std::collections::HashMap::new(), 0)
            };

        let height = self.block_height()?;
        // Iterate through every block from the snapshot height (or genesis) to the tip.
        for i in start_height..=height {
            if let Some(block) = self.get_block_by_index(i)? {
                for tx in &block.transactions {
                    // For regular transactions, remove the inputs they spend from the UTXO set.
                    if !tx.is_coinbase() {
                        for input in &tx.inputs {
                            new_utxos.remove(&input.outpoint);
                        }
                    }

                    // Add all new outputs from this transaction to the UTXO set.
                    let txid = tx.txid()?;
                    for (vout, output) in tx.outputs.iter().enumerate() {
                        let outpoint = OutPoint {
                            txid,
                            vout: vout as u32,
                        };
                        new_utxos.insert(outpoint, (false, output.clone()));
                    }
                }

                // Periodically save a new snapshot to avoid long rebuilds next time.
                if i > 0 && i % Self::UTXO_SNAPSHOT_INTERVAL == 0 {
                    self.save_utxo_snapshot(i)?;
                }
            }
        }

        // After rebuilding the UTXO set, iterate through the mempool and mark spent UTXOs.
        // This ensures the in-memory UTXO state is consistent with pending transactions.
        for (_, tx, _) in self.mempool.values() {
            for input in &tx.inputs {
                if let Some(utxo_entry) = new_utxos.get_mut(&input.outpoint) {
                    utxo_entry.0 = true; // Mark as spent in mempool
                }
            }
        }

        // Replace the old in-memory UTXO set with the newly built one.
        self.utxo_set.utxos = new_utxos;

        // Save a final snapshot at the current tip if it's been a while.
        if height > 0 && height % Self::UTXO_SNAPSHOT_INTERVAL != 0 {
            self.save_utxo_snapshot(height)?;
        }
        Ok(())
    }
}
