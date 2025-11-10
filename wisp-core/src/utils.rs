use crate::{
    blockchain::Block,
    currency::Amount,
    sha256::Hash,
    signatures::PublicKey,
    transactions::{OutPoint, Transaction, TransactionInput, TransactionOutput},
};

use anyhow::{Context, Result as AnyhowResult};
use bincode::{config::standard as bincode_config, Decode, Encode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Error as IoError, ErrorKind as IoErrorKind, Read, Result as IoResult, Write},
};

/// A wrapper around a `Hash` to represent the root of a Merkle tree.
#[derive(Encode, Decode, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MerkleRoot(pub Hash);

impl MerkleRoot {
    /// Calculates the Merkle root for a list of transactions using a binary Merkle tree
    /// with double-SHA256 hashing.
    /// It repeatedly hashes pairs of hashes in a layer until only one root hash remains.
    pub fn calculate(transactions: &[Transaction]) -> Result<MerkleRoot, anyhow::Error> {
        let mut layer: Vec<Hash> = vec![];

        for transaction in transactions {
            // Use the witness transaction ID (wtxid) for the Merkle root.
            // This prevents transaction malleability attacks.
            layer.push(transaction.wtxid()?);
        }

        if layer.is_empty() {
            return Ok(MerkleRoot(Hash::zero()));
        }

        // Continue hashing until only one hash (the root) is left.
        while layer.len() > 1 {
            // Pre-allocate with capacity to avoid reallocations in the loop.
            let mut new_layer = Vec::with_capacity(layer.len() / 2 + 1);
            // Process hashes in pairs.
            for pair in layer.chunks(2) {
                let left = pair[0];
                // If there's an odd number of hashes, duplicate the last one.
                let right = pair.get(1).unwrap_or(&pair[0]);

                // Use the canonical hash function, which correctly handles double-SHA256.
                // We explicitly create a slice `&[Hash]` to satisfy the `Hashable` trait bound.
                let hashes_to_combine: &[Hash] = &[left, *right];
                new_layer.push(crate::sha256::hash(hashes_to_combine));
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

    let genesis_message = "Sic Mundus Creatus Est // 5.11.2025 //";
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
    let genesis_timestamp = DateTime::parse_from_rfc3339("2025-11-05T14:47:02.192361230Z")
        .context("Invalid hardcoded genesis timestamp")?
        .with_timezone(&Utc);

    let genesis_block = Block::new(
        1,
        genesis_timestamp,
        1030674,
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
    fn save<O: Write>(&self, writer: O) -> IoResult<()>;
    fn load<I: Read>(reader: I) -> IoResult<Self>;

    fn save_to_file<P: AsRef<std::path::Path>>(&self, path: P) -> IoResult<()> {
        use std::io::BufWriter;
        let file = File::create(&path)?;
        self.save(BufWriter::new(file))
    }

    fn load_from_file<P: AsRef<std::path::Path>>(path: P) -> IoResult<Self> {
        // Open the file and wrap it in a BufReader for efficiency,
        // which is common practice and also implements the required traits.
        use std::io::BufReader;
        let file = File::open(&path)?;
        let reader = BufReader::new(file);
        Self::load(reader)
    }
}

impl<T> Saveable for T
where
    T: Encode + Decode<()> + Sized,
{
    fn save<O: Write>(&self, mut writer: O) -> IoResult<()> {
        bincode::encode_into_std_write(self, &mut writer, bincode_config())
            .map(|_| ())
            .map_err(|e| {
                IoError::new(
                    IoErrorKind::InvalidData,
                    format!("Failed to save with bincode: {}", e),
                )
            })
    }

    fn load<I: Read>(mut reader: I) -> IoResult<Self> {
        bincode::decode_from_std_read(&mut reader, bincode_config()).map_err(|e| {
            IoError::new(
                IoErrorKind::InvalidData,
                format!("Failed to load with bincode: {}", e),
            )
        })
    }
}
