use crate::{
    blockchain::Block,
    currency::Amount,
    sha256::Hash,
    signatures::PublicKey,
    transactions::{OutPoint, Script, Transaction, TransactionInput, TransactionOutput},
};

use anyhow::{Context, Result as AnyhowResult};
use bincode::{config::standard as bincode_config, Decode, Encode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Error as IoError, ErrorKind as IoErrorKind, Read, Result as IoResult, Write},
};

/// Represents the root of a Merkle tree of transactions.
#[derive(Encode, Decode, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MerkleRoot(pub Hash);

impl MerkleRoot {
    pub fn calculate(transactions: &[Transaction]) -> Result<MerkleRoot, anyhow::Error> {
        let mut layer: Vec<Hash> = vec![];

        for transaction in transactions {
            // Use txid (malleability resistant) for the header Merkle Root
            layer.push(transaction.txid()?);
        }

        if layer.is_empty() {
            return Ok(MerkleRoot(Hash::zero()));
        }

        while layer.len() > 1 {
            let mut new_layer = Vec::with_capacity(layer.len() / 2 + 1);
            for pair in layer.chunks(2) {
                let left = pair[0];
                let right = pair.get(1).unwrap_or(&pair[0]);
                let hashes_to_combine: &[Hash] = &[left, *right];

                new_layer.push(crate::sha256::hash(hashes_to_combine));
            }
            layer = new_layer;
        }

        Ok(MerkleRoot(layer[0]))
    }
}

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

pub fn calculate_expected_supply(height: u64) -> Amount {
    let mut total_supply = 0u64;
    let mut current_reward = crate::INITIAL_BLOCK_REWARD_SMALLEST_UNITS;
    let mut remaining_blocks = height + 1;

    while remaining_blocks > 0 && current_reward > 0 {
        let blocks_in_this_era = std::cmp::min(remaining_blocks, crate::HALVING_INTERVAL);
        let era_supply = current_reward.saturating_mul(blocks_in_this_era);
        total_supply = total_supply.saturating_add(era_supply);

        remaining_blocks -= blocks_in_this_era;
        current_reward /= 2;
    }

    Amount::from_smallest_unit(total_supply)
}

/// Constructs the genesis block of the blockchain.
///
/// This block is hardcoded with a specific message, timestamp, and other parameters.
/// It serves as the immutable foundation of the entire chain.
pub fn genesis_block() -> AnyhowResult<Block> {
    let genesis_pubkey_hex = "020000000000000000000000000000000000000000000000000000000000000001";
    let genesis_pubkey_bytes =
        hex::decode(genesis_pubkey_hex).context("Failed to decode genesis burn public key hex")?;
    let genesis_verifying_key = k256::ecdsa::VerifyingKey::from_sec1_bytes(&genesis_pubkey_bytes)
        .context("Failed to create verifying key from genesis bytes")?;

    let genesis_message = "Sic Mundus Creatus Est // 5.11.2025 //";
    let mut coinbase_data = Vec::new();
    coinbase_data.extend_from_slice(&0u64.to_le_bytes());
    coinbase_data.extend_from_slice(genesis_message.as_bytes());

    let coinbase_tx = Transaction::new(
        vec![TransactionInput {
            outpoint: OutPoint {
                txid: Hash::zero(),
                vout: u32::MAX,
            },
            signature: None,
            public_key: None,
            redeem_script: None,
            coinbase_data: Some(coinbase_data),
        }],
        vec![TransactionOutput {
            value: Amount::from_smallest_unit(crate::INITIAL_BLOCK_REWARD_SMALLEST_UNITS),
            script: Script::Classic(PublicKey(genesis_verifying_key)),
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
