pub mod blockchain;
pub mod currency;

pub mod mempool;
pub mod network;
pub mod pow;
pub mod query;
pub mod reorg;
pub mod sha256;
pub mod signatures;
pub mod storage;
pub mod transactions;
pub mod utils;
pub mod utxo;
pub mod validation;

use bincode::{Decode, Encode};
use serde::{Deserialize, Serialize};
use uint::construct_uint;

// Defines a 256-bit unsigned integer type `U256`.
construct_uint! {
   #[derive(Encode, Decode, Serialize, Deserialize)]
   pub struct U256(4);
}

/// The initial block reward in the smallest currency unit (e.g., satoshis).
/// 100 WISP * 10^8.
pub const INITIAL_BLOCK_REWARD_SMALLEST_UNITS: u64 =
    100 * 10u64.pow(currency::Amount::DECIMAL_PLACES);

/// With a 2-minute block time, 525,600 blocks equates to approximately 2 years.
/// (30 blocks/hour * 24 hours/day * 365 days/year * 2 years = 525,600)
pub const HALVING_INTERVAL: u64 = 525_600;

/// The ideal time between blocks in seconds. Used for difficulty adjustment.
pub const IDEAL_BLOCK_TIME: u64 = 120; // 2 minutes in seconds

/// The window of blocks used for difficulty adjustment. 720 blocks represents 1 day.
pub const DAA_WINDOW: usize = 720;

/// The maximum value for the proof-of-work target (lowest difficulty).
pub const MAX_TARGET: U256 = U256([
    0xFFFF_FFFF_FFFF_FFFF,
    0xFFFF_FFFF_FFFF_FFFF,
    0xFFFF_FFFF_FFFF_FFFF,
    0x0000_00FF_FFFF_FFFF,
]);

/// The minimum value for the proof-of-work target (highest difficulty).
pub const MIN_TARGET: U256 = U256([1, 0, 0, 0]);

/// The maximum number of transactions allowed in a single block.
pub const MAX_BLOCK_TRANSACTIONS: usize = 1000;

/// The maximum size of a network message in bytes.
pub const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024; // 10 MB

/// The maximum allowed timestamp difference in seconds for a block from the future.
pub const MAX_BLOCK_FUTURE_TIMESTAMP: u64 = 600; // 10 minutes in seconds

#[cfg(test)]
mod tests {
    use crate::{
        currency::Amount,
        network::Message,
        signatures::{PrivateKey, Signature},
        transactions::OutPoint,
        transactions::{Transaction, TransactionInput, TransactionOutput},
        utils,
        utxo::UtxoSet,
    };
    use bincode::{config::standard as bincode_config, Decode, Encode};

    fn assert_roundtrip<T>(value: T)
    where
        T: Encode + Decode<()> + std::fmt::Debug + PartialEq,
    {
        let encoded = bincode::encode_to_vec(&value, bincode_config()).unwrap();
        let (decoded, len) = bincode::decode_from_slice(&encoded, bincode_config()).unwrap();
        assert_eq!(value, decoded);
        assert_eq!(encoded.len(), len);
    }

    #[test]
    fn test_private_key_serialization_roundtrip() {
        let private_key =
            PrivateKey::generate_keypair_with_rng(&mut ecdsa::signature::rand_core::OsRng);
        assert_roundtrip(private_key);
    }

    #[test]
    fn test_public_key_serialization_roundtrip() {
        let private_key =
            PrivateKey::generate_keypair_with_rng(&mut ecdsa::signature::rand_core::OsRng);
        let public_key = private_key.public_key();
        assert_roundtrip(public_key);
    }

    #[test]
    fn test_signature_serialization_roundtrip() {
        let private_key =
            PrivateKey::generate_keypair_with_rng(&mut ecdsa::signature::rand_core::OsRng);
        let hash = crate::sha256::hash(&b"test message"[..]);
        let signature = Signature::sign_transaction_hash(&hash, &private_key);
        assert_roundtrip(signature);
    }

    #[test]
    fn test_genesis_block_serialization_roundtrip() {
        let genesis_block = utils::genesis_block().unwrap();
        assert_roundtrip(genesis_block);
    }

    #[test]
    fn test_message_serialization_roundtrip() {
        let private_key =
            PrivateKey::generate_keypair_with_rng(&mut ecdsa::signature::rand_core::OsRng);
        let public_key = private_key.public_key();
        let tx = Transaction::new(
            vec![TransactionInput::default()],
            vec![TransactionOutput {
                value: Amount::from_smallest_unit(100),
                pubkey: public_key,
            }],
        );
        let message = Message::SubmitTransaction(tx);
        assert_roundtrip(message);
    }

    #[test]
    fn test_utxo_set_serialization_roundtrip() {
        let mut utxo_set = UtxoSet::new();
        let private_key =
            PrivateKey::generate_keypair_with_rng(&mut ecdsa::signature::rand_core::OsRng);
        let tx = Transaction::new(
            vec![], // Dummy tx
            vec![TransactionOutput {
                value: Amount::from_smallest_unit(500),
                pubkey: private_key.public_key(),
            }],
        );
        let outpoint = OutPoint {
            txid: tx.txid().unwrap(),
            vout: 0,
        };
        // The tuple is (is_spent_in_mempool, output)
        utxo_set
            .utxos
            .insert(outpoint, (false, tx.outputs[0].clone()));
        assert_roundtrip(utxo_set);
    }
}
