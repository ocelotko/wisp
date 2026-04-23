use crate::blockchain::Blockchain;
use crate::storage::DBKeys;
use crate::transactions::Script;
use anyhow::Result;
use bincode::config::standard as bincode_config;
use log::info;
use std::collections::HashMap;

impl Blockchain {
    /// Scans the entire UTXO set and populates the Address-to-OutPoint index.
    /// This only needs to be run once when updating from a version that didn't have the index.
    pub fn migrate_rebuild_address_index(&mut self) -> Result<()> {
        info!("MIGRATION: Rebuilding Address-to-OutPoint index from existing UTXO set...");

        // 1. Group all current UTXOs by their address/script hash
        let mut address_map: HashMap<String, Vec<crate::transactions::OutPoint>> = HashMap::new();

        for (outpoint, output) in &self.utxo_set.utxos {
            let addr_id = match &output.script {
                Script::Classic(pk) => pk.fingerprint(),
                Script::Shadow(h)
                | Script::ShadowScript(h)
                | Script::Aurora(h)
                | Script::AuroraScript(h) => h.to_string(),
            };
            address_map.entry(addr_id).or_default().push(*outpoint);
        }

        // 2. Write them to the database in a single batch
        let mut batch = sled::Batch::default();
        let total_addresses = address_map.len();

        for (id, outpoints) in address_map {
            let key = DBKeys::address_utxo(&id);
            let val = bincode::encode_to_vec(&outpoints, bincode_config())?;
            batch.insert(key, val);
        }

        self.db.apply_batch(batch)?;

        info!(
            "MIGRATION COMPLETE: Indexed {} outputs across {} unique addresses.",
            self.utxo_set.utxos.len(),
            total_addresses
        );
        Ok(())
    }
}
