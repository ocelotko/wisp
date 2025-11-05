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

/// The current version for newly created blocks.
pub const BLOCK_VERSION: u32 = 1;

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
/// This acts as a secondary limit to prevent DoS with many tiny, valid transactions.
/// 4000 is a safe upper bound, as a 1MB block can't hold more simple txs than this.
pub const MAX_BLOCK_TRANSACTIONS: usize = 4000;

/// The maximum size of a block in bytes.
pub const MAX_BLOCK_SIZE_BYTES: usize = 1_000_000; // 1 MB

/// The maximum size of a single transaction in bytes.
pub const MAX_TRANSACTION_SIZE_BYTES: usize = 100_000; // 100 KB

/// The maximum size of a network message in bytes. Should be slightly larger than
/// the max block size to account for message overhead.
pub const MAX_MESSAGE_SIZE: usize = 2 * 1024 * 1024; // 2 MB

/// The maximum allowed timestamp difference in seconds for a block from the future.
pub const MAX_BLOCK_FUTURE_TIMESTAMP: u64 = 180; // 3 minutes in seconds
