use anyhow::anyhow;
use anyhow::Context;
use bincode::{
    de::{read::Reader, Decoder},
    enc::{write::Writer, Encoder},
    error::{DecodeError, EncodeError},
    Decode, Encode,
};
use ecdsa::{
    signature::{Signer, Verifier},
    Signature as EcdsaSignature, SigningKey, VerifyingKey,
};
use k256::{ecdsa::signature::rand_core::CryptoRngCore, Secp256k1};
use serde::{de::Error as DeError, Deserialize, Deserializer, Serialize, Serializer};

use std::{
    hash::{Hash as StdHash, Hasher},
    str::FromStr,
};

use crate::sha256::{Hash, Hashable};

/// A wrapper around a `k256::ecdsa::Signature` to provide serialization and domain-specific methods.
#[derive(Encode, Decode, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Signature(#[bincode(with_serde)] pub EcdsaSignature<Secp256k1>);

impl Signature {
    /// Creates a new signature from a DER-encoded hex string.
    pub fn from_hex(s: &str) -> anyhow::Result<Self> {
        let bytes = hex::decode(s)?;
        EcdsaSignature::from_der(&bytes)
            .map(Signature)
            .map_err(|e| anyhow!("Failed to create Signature from DER bytes: {}", e))
    }

    /// Returns the signature as a DER-encoded hex string.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0.to_der().as_bytes())
    }

    /// Creates a new signature for a given transaction hash using a private key.
    ///
    /// All signatures are generated using deterministic ECDSA per RFC 6979;
    /// no external randomness is required for the signing operation itself.
    pub fn sign_transaction_hash(transaction_hash: &Hash, private_key: &PrivateKey) -> Self {
        let signing_key = &private_key.0;
        let signature = signing_key.sign(&transaction_hash.as_bytes()[..]);
        Signature(signature)
    }

    /// Verifies that the signature is valid for a given transaction hash and public key.
    pub fn verify_transaction_hash(&self, transaction_hash: &Hash, public_key: &PublicKey) -> bool {
        public_key
            .0
            .verify(&transaction_hash.as_bytes()[..], &self.0)
            .is_ok()
    }
}

/// A wrapper around a `k256::ecdsa::VerifyingKey` representing a secp256k1 public key.
#[derive(
    Encode, Decode, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Copy,
)]
pub struct PublicKey(#[bincode(with_serde)] pub VerifyingKey<Secp256k1>);

impl Default for PublicKey {
    /// Creates a default `PublicKey`.
    ///
    /// This uses the hardcoded "burn" public key from the genesis block utility.
    /// This is necessary because `VerifyingKey` itself does not have a natural default.
    fn default() -> Self {
        let genesis_pubkey_hex =
            "020000000000000000000000000000000000000000000000000000000000000001";
        let genesis_pubkey_bytes =
            hex::decode(genesis_pubkey_hex).expect("Failed to decode constant genesis pubkey hex");
        let genesis_verifying_key = VerifyingKey::from_sec1_bytes(&genesis_pubkey_bytes).unwrap();
        PublicKey(genesis_verifying_key)
    }
}

use sha2::Digest;
/// Implements `Hashable` for `PublicKey` to allow it to be included in hashed data structures.
impl Hashable for PublicKey {
    fn update_hasher(&self, hasher: &mut sha2::Sha256) {
        hasher.update(self.0.to_encoded_point(true).as_bytes());
    }
}

impl FromStr for PublicKey {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = hex::decode(s).context("Failed to hex-decode public key string")?;
        VerifyingKey::from_sec1_bytes(&bytes)
            .map(PublicKey)
            .map_err(|e| anyhow!("Failed to create VerifyingKey from SEC1 bytes: {}", e))
    }
}

impl PublicKey {
    /// Returns the compressed SEC1-encoded public key as a hex string.
    /// This is commonly used as the wallet address.
    pub fn fingerprint(&self) -> String {
        let encoded_point = self.0.to_encoded_point(true);
        hex::encode(encoded_point.as_bytes())
    }

    /// Returns the compressed SEC1-encoded public key as a 33-byte array.
    pub fn to_bytes(&self) -> [u8; 33] {
        self.0
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .expect("EncodedPoint should be 33 bytes for compressed secp256k1 keys")
    }
}

impl StdHash for PublicKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let encoded_point = self.0.to_encoded_point(true);
        StdHash::hash(&encoded_point.as_bytes(), state);
    }
}

/// A wrapper around a `k256::ecdsa::SigningKey` representing a secp256k1 private key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateKey(pub SigningKey<Secp256k1>);

impl Serialize for PrivateKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&self.0.to_bytes())
    }
}

impl<'de> Deserialize<'de> for PrivateKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes: &[u8] = serde_bytes::deserialize(deserializer)?;
        SigningKey::from_slice(bytes)
            .map(PrivateKey)
            .map_err(D::Error::custom)
    }
}

impl Encode for PrivateKey {
    fn encode<E: Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        encoder.writer().write(&self.0.to_bytes())
    }
}

impl Decode<()> for PrivateKey {
    fn decode<D: Decoder>(decoder: &mut D) -> Result<Self, DecodeError> {
        // The private key is 32 bytes.
        // We need to read into a fixed-size array.
        let mut bytes = [0u8; 32];
        decoder.reader().read(&mut bytes)?;
        SigningKey::from_slice(&bytes)
            .map(PrivateKey)
            .map_err(|e| DecodeError::OtherString(format!("Failed to decode private key: {}", e)))
    }
}

impl PrivateKey {
    /// Generates a new, random private key using the provided cryptographically secure random number generator.
    pub fn generate_keypair_with_rng<R>(rng: &mut R) -> Self
    where
        R: CryptoRngCore,
    {
        PrivateKey(SigningKey::random(rng))
    }

    /// Derives the corresponding public key from this private key.
    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.0.verifying_key().clone())
    }

    /// Returns the private key as a 32-byte hex string.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0.to_bytes())
    }
}

impl FromStr for PrivateKey {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = hex::decode(s).context("Failed to hex-decode private key string")?;
        SigningKey::from_slice(&bytes)
            .map(PrivateKey)
            .map_err(|e| anyhow!("Failed to create SigningKey from bytes: {}", e))
    }
}
