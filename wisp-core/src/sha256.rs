use crate::{utils::MerkleRoot, U256};
use bincode::{Decode, Encode};
use hex;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use sha2::Digest;
pub use sha2::Sha256;
use std::{convert::TryFrom, fmt};

/// A trait for objects that can be hashed in a standardized, consensus-critical way.
/// This ensures that all parts of the codebase agree on how to serialize an object for hashing.
pub trait Hashable {
    /// Updates a hasher with the object's consensus-critical byte representation.
    fn update_hasher(&self, hasher: &mut Sha256);
}

/// A trait for objects that can be hashed including their witness data.
/// This is separate from `Hashable` to distinguish between txid and wtxid.
pub trait WitnessHashable {
    fn update_witness_hasher(&self, hasher: &mut Sha256);
}

impl Hashable for u32 {
    fn update_hasher(&self, hasher: &mut Sha256) {
        hasher.update(&self.to_be_bytes());
    }
}

impl Hashable for u64 {
    fn update_hasher(&self, hasher: &mut Sha256) {
        hasher.update(&self.to_be_bytes());
    }
}

impl Hashable for Hash {
    fn update_hasher(&self, hasher: &mut Sha256) {
        hasher.update(self.as_bytes());
    }
}

impl Hashable for MerkleRoot {
    fn update_hasher(&self, hasher: &mut Sha256) {
        self.0.update_hasher(hasher);
    }
}

impl Hashable for U256 {
    fn update_hasher(&self, hasher: &mut Sha256) {
        hasher.update(&self.to_big_endian());
    }
}

impl Hashable for [Hash] {
    fn update_hasher(&self, hasher: &mut Sha256) {
        for h in self {
            h.update_hasher(hasher);
        }
    }
}

impl Hashable for [u8] {
    fn update_hasher(&self, hasher: &mut Sha256) {
        hasher.update(self);
    }
}

/// A private, generic function that performs the core double-SHA256 hashing logic.
/// It takes a closure that defines how to update the hasher, allowing it to work
/// with both `Hashable` and `WitnessHashable` traits without code duplication.
fn double_sha256_hash<F>(update_fn: F) -> Hash
where
    F: FnOnce(&mut Sha256),
{
    let mut hasher = Sha256::new();
    update_fn(&mut hasher);
    let first_pass = hasher.finalize();

    let mut hasher2 = Sha256::new();
    hasher2.update(&first_pass);
    let digest = hasher2.finalize();

    let hash_bytes: [u8; 32] = digest.into();
    Hash::from_u256(crate::U256::from_big_endian(&hash_bytes))
}

/// Performs a standardized double-SHA256 hash on any object that implements the `Hashable` trait.
/// This is the single, canonical hashing function that should be used for all consensus-critical hashing.
pub fn hash<T: Hashable + ?Sized>(data: &T) -> Hash {
    double_sha256_hash(|hasher| data.update_hasher(hasher))
}

/// Performs a double-SHA256 hash on an object's witness data.
pub fn witness_hash<T: WitnessHashable + ?Sized>(data: &T) -> Hash {
    double_sha256_hash(|hasher| data.update_witness_hasher(hasher))
}

/// A wrapper around a `U256` to represent a 256-bit SHA-256 hash.
#[serde_as]
#[derive(
    Encode, Decode, Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Hash(crate::U256);

impl Hash {
    /// Checks if the hash is less than or equal to the given PoW target.
    pub fn matches_target(&self, target: U256) -> bool {
        self.0 <= target
    }

    /// Creates a `Hash` from a 32-byte array.
    pub fn from_bytes(bytes: &[u8; 32]) -> Self {
        Hash(U256::from_big_endian(bytes))
    }

    /// Returns a hash of all zeros.
    pub fn zero() -> Self {
        Hash(U256::zero())
    }

    /// Returns the big-endian byte representation of the hash.
    pub fn as_bytes(&self) -> [u8; 32] {
        self.0.to_big_endian()
    }

    /// Creates a `Hash` from a `U256`. This is private to ensure hashes are only created via the `hash` function.
    fn from_u256(u: U256) -> Self {
        Hash(u)
    }
}

impl From<Hash> for String {
    fn from(hash: Hash) -> Self {
        hex::encode(hash.as_bytes())
    }
}

impl TryFrom<&str> for Hash {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        let bytes = hex::decode(s).map_err(|e| e.to_string())?;

        if bytes.len() != 32 {
            return Err(format!(
                "Invalid hex string length: expected 64 chars (32 bytes), found {} bytes",
                bytes.len()
            ));
        }

        let mut array = [0u8; 32];
        array.copy_from_slice(&bytes);

        let u256 = U256::from_big_endian(&array);
        Ok(Hash(u256))
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", hex::encode(self.as_bytes()))
    }
}
