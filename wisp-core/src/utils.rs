use crate::{
    blockchain::Block,
    currency::Amount,
    sha256::{Hash, Hashable},
    signatures::PublicKey,
    transactions::{OutPoint, Transaction, TransactionInput, TransactionOutput},
};

use anyhow::{Context, Result as AnyhowResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::{
    fs::File,
    io::{Read, Result as IoResult, Write},
};

/// A wrapper around a `Hash` to represent the root of a Merkle tree.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MerkleRoot(pub Hash);

impl MerkleRoot {
    /// Calculates the Merkle root for a list of transactions.
    /// It repeatedly hashes pairs of hashes in a layer until only one root hash remains.
    pub fn calculate(transactions: &[Transaction]) -> Result<MerkleRoot, anyhow::Error> {
        let mut layer: Vec<Hash> = vec![];

        for transaction in transactions {
            layer.push(transaction.wtxid()?);
        }

        if layer.is_empty() {
            return Ok(MerkleRoot(Hash::zero()));
        }

        // Continue hashing until only one hash (the root) is left.
        while layer.len() > 1 {
            let mut new_layer = vec![];
            // Process hashes in pairs.
            for pair in layer.chunks(2) {
                let left = pair[0];
                // If there's an odd number of hashes, duplicate the last one.
                let right = pair.get(1).unwrap_or(&pair[0]);

                let mut combined_hasher = crate::sha256::Sha256::new();
                left.update_hasher(&mut combined_hasher);
                right.update_hasher(&mut combined_hasher);

                // Perform the second hash of the double-SHA256 manually.
                // This is more direct and avoids the deprecated `as_slice` method.
                let first_pass = combined_hasher.finalize();
                let mut second_hasher = crate::sha256::Sha256::new();
                second_hasher.update(&first_pass);
                let combined_hash = Hash::from_bytes(&second_hasher.finalize().into());
                new_layer.push(combined_hash);
            }
            layer = new_layer;
        }

        Ok(MerkleRoot(layer[0]))
    }
}

/// Calculates the block reward for a given block height.
/// The reward is halved every `HALVING_INTERVAL` blocks.
pub fn calculate_block_reward(block_height: u64) -> Amount {
    let halvings = block_height / crate::HALVING_INTERVAL;

    if halvings >= 64 {
        Amount::zero()
    } else {
        let reward_in_smallest_units = (crate::INITIAL_BLOCK_REWARD_SMALLEST_UNITS)
            .checked_shr(halvings as u32)
            .unwrap_or(0);

        Amount::from_smallest_unit(reward_in_smallest_units)
    }
}

/// Constructs the genesis block of the blockchain.
/// This block is hardcoded and serves as the foundation of the chain.
pub fn genesis_block() -> AnyhowResult<Block> {
    let genesis_pubkey_hex = "020000000000000000000000000000000000000000000000000000000000000001";
    let genesis_pubkey_bytes =
        hex::decode(genesis_pubkey_hex).context("Failed to decode genesis burn public key hex")?;
    let genesis_verifying_key = k256::ecdsa::VerifyingKey::from_sec1_bytes(&genesis_pubkey_bytes)
        .context("Failed to create verifying key from genesis bytes")?;

    let genesis_message = "Sic Mundus Creatus Est // 16.10.2025 //";
    let mut coinbase_data = Vec::new();
    coinbase_data.extend_from_slice(&0u64.to_le_bytes()); // Block height 0
    coinbase_data.extend_from_slice(genesis_message.as_bytes());

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
            value: Amount::from_smallest_unit(crate::INITIAL_BLOCK_REWARD_SMALLEST_UNITS),
            pubkey: PublicKey(genesis_verifying_key),
        }],
    );

    let merkle_root = MerkleRoot::calculate(&[coinbase_tx.clone()])?;
    let genesis_timestamp = DateTime::parse_from_rfc3339("2025-10-16T20:50:17.261079162Z")
        .unwrap()
        .with_timezone(&Utc);

    let genesis_block = Block::new(
        1,
        genesis_timestamp,
        1624937,
        Hash::zero(),
        merkle_root,
        crate::MAX_TARGET,
        0,
        vec![coinbase_tx],
    );

    Ok(genesis_block)
}

/// A trait for objects that can be saved to and loaded from a stream or file.
pub trait Saveable
where
    Self: Sized,
{
    fn load<I: Read>(reader: I) -> IoResult<Self>;
    fn save<O: Write>(&self, writer: O) -> IoResult<()>;
    fn save_to_file<P: AsRef<std::path::Path>>(&self, path: P) -> IoResult<()> {
        let file = File::create(&path)?;
        self.save(file)
    }

    fn load_from_file<P: AsRef<std::path::Path>>(path: P) -> IoResult<Self> {
        let file = File::open(&path)?;
        Self::load(file)
    }
}
