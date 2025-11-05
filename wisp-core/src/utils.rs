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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sha256::hash;
    use crate::transactions::Transaction;
    use crate::HALVING_INTERVAL;
    use crate::INITIAL_BLOCK_REWARD_SMALLEST_UNITS;

    #[test]
    fn test_merkle_root_calculation() {
        // 1. Empty list
        let root_empty = MerkleRoot::calculate(&[]).unwrap();
        assert_eq!(root_empty.0, Hash::zero());

        // 2. Single transaction
        let tx1 = Transaction::new(vec![], vec![]);
        let wtxid1 = tx1.wtxid().unwrap();
        let root_one = MerkleRoot::calculate(&[tx1.clone()]).unwrap();
        assert_eq!(root_one.0, wtxid1);

        // 3. Even number of transactions (2)
        let tx2 = Transaction::new(vec![], vec![]); // Different tx
        let wtxid2 = tx2.wtxid().unwrap();
        let combined_hash = hash(&[wtxid1, wtxid2][..]);
        let root_two = MerkleRoot::calculate(&[tx1.clone(), tx2.clone()]).unwrap();
        assert_eq!(root_two.0, combined_hash);

        // 4. Odd number of transactions (3)
        let tx3 = Transaction::new(vec![], vec![]);
        let wtxid3 = tx3.wtxid().unwrap();
        let combined_1_2 = hash(&[wtxid1, wtxid2][..]);
        let combined_3_3 = hash(&[wtxid3, wtxid3][..]); // Duplicated
        let final_root_hash = hash(&[combined_1_2, combined_3_3][..]);
        let root_three = MerkleRoot::calculate(&[tx1, tx2, tx3]).unwrap();
        assert_eq!(root_three.0, final_root_hash);
    }

    #[test]
    fn test_block_reward_halving() {
        // Block 0 (first block)
        assert_eq!(
            calculate_block_reward(0).as_smallest_unit(),
            INITIAL_BLOCK_REWARD_SMALLEST_UNITS
        );

        // Block just before first halving
        assert_eq!(
            calculate_block_reward(HALVING_INTERVAL - 1).as_smallest_unit(),
            INITIAL_BLOCK_REWARD_SMALLEST_UNITS
        );

        // Block at first halving
        assert_eq!(
            calculate_block_reward(HALVING_INTERVAL).as_smallest_unit(),
            INITIAL_BLOCK_REWARD_SMALLEST_UNITS / 2
        );

        // Block at second halving
        assert_eq!(
            calculate_block_reward(HALVING_INTERVAL * 2).as_smallest_unit(),
            INITIAL_BLOCK_REWARD_SMALLEST_UNITS / 4
        );

        // After 64 halvings, reward should be 0
        assert_eq!(
            calculate_block_reward(HALVING_INTERVAL * 64).as_smallest_unit(),
            0
        );
        assert_eq!(
            calculate_block_reward(HALVING_INTERVAL * 100).as_smallest_unit(),
            0
        );
    }

    #[test]
    fn test_genesis_block_is_deterministic() {
        let genesis1 = genesis_block().unwrap();
        let genesis2 = genesis_block().unwrap();
        let genesis_hash = genesis1.id().unwrap();

        // Ensure two calls produce the exact same block
        assert_eq!(genesis1, genesis2);

        // Check against a known, hardcoded hash to prevent accidental changes
        let expected_genesis_hash =
            Hash::try_from("000000179bf3dc1f7dd18b7e7a9c85d81dfe028afd04c52aa0459152d50bbe86")
                .unwrap();
        assert_eq!(genesis_hash, expected_genesis_hash);
    }

    #[test]
    fn test_saveable_trait_roundtrip() {
        #[derive(Encode, Decode, PartialEq, Debug)]
        struct TestStruct {
            a: u32,
            b: String,
        }

        let original = TestStruct {
            a: 42,
            b: "hello world".to_string(),
        };

        // Test in-memory roundtrip
        let mut buffer: Vec<u8> = Vec::new();
        original.save(&mut buffer).unwrap();
        let loaded = TestStruct::load(&buffer[..]).unwrap();

        assert_eq!(original, loaded);
    }
}
