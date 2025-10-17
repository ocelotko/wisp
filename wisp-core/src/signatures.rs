use anyhow::anyhow;
use anyhow::Context;
use ecdsa::{
    signature::{Signer, Verifier},
    Signature as ECDSASignature, SigningKey, VerifyingKey,
};
use k256::Secp256k1;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

use std::io::{Error as IoError, ErrorKind as IoErrorKind, Read, Result as IoResult, Write};
use std::{
    hash::{Hash as StdHash, Hasher},
    str::FromStr,
};

use crate::sha256::{Hash, Hashable};
use crate::utils::Saveable;

/// A wrapper around a `k256::ecdsa::Signature` to provide domain-specific methods.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signature(pub ECDSASignature<Secp256k1>);

impl Signature {
    /// Creates a new signature for a given transaction hash using a private key.
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

/// A wrapper around a `k256::ecdsa::VerifyingKey` representing a public key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PublicKey(#[serde(with = "pubkey_serde")] pub VerifyingKey<Secp256k1>);

use sha2::Digest;
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
    /// Returns the compressed SEC1-encoded public key as a hex string. This is used as the wallet address.
    pub fn fingerprint(&self) -> String {
        let encoded_point = self.0.to_encoded_point(true);
        hex::encode(encoded_point.as_bytes())
    }
}

impl StdHash for PublicKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let encoded_point = self.0.to_encoded_point(true);
        StdHash::hash(&encoded_point.as_bytes(), state);
    }
}

/// A wrapper around a `k256::ecdsa::SigningKey` representing a private key.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PrivateKey(#[serde(with = "signkey_serde")] pub SigningKey<Secp256k1>);

impl PrivateKey {
    /// Generates a new, random private key.
    pub fn generate_keypair() -> Self {
        PrivateKey(SigningKey::random(&mut OsRng))
    }

    /// Derives the corresponding public key from this private key.
    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.0.verifying_key().clone())
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

impl Saveable for PrivateKey {
    fn load<I: Read>(reader: I) -> IoResult<Self> {
        bincode::deserialize_from(reader).map_err(|e| {
            IoError::new(
                IoErrorKind::InvalidData,
                format!("Failed to deserialize PrivateKey with bincode: {}", e),
            )
        })
    }

    fn save<O: Write>(&self, writer: O) -> IoResult<()> {
        bincode::serialize_into(writer, self).map_err(|e| {
            IoError::new(
                IoErrorKind::InvalidData,
                format!("Failed to serialize PrivateKey with bincode: {}", e),
            )
        })
    }
}

impl Saveable for PublicKey {
    fn load<I: Read>(reader: I) -> IoResult<Self> {
        bincode::deserialize_from(reader).map_err(|e| {
            IoError::new(
                IoErrorKind::InvalidData,
                format!("Failed to deserialize PublicKey with bincode: {}", e),
            )
        })
    }

    fn save<O: Write>(&self, writer: O) -> IoResult<()> {
        bincode::serialize_into(writer, self).map_err(|e| {
            IoError::new(
                IoErrorKind::InvalidData,
                format!("Failed to serialize PublicKey with bincode: {}", e),
            )
        })
    }
}

/// A custom serde module for serializing and deserializing `SigningKey` as raw bytes.
mod signkey_serde {
    use serde::Deserialize;
    pub fn serialize<S>(
        key: &super::SigningKey<super::Secp256k1>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_bytes(&key.to_bytes())
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<super::SigningKey<super::Secp256k1>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let bytes: Vec<u8> = Vec::<u8>::deserialize(deserializer)?;
        super::SigningKey::from_slice(&bytes).map_err(|e| {
            serde::de::Error::custom(format!("Failed to create SigningKey from bytes: {}", e))
        })
    }
}

/// A custom serde module for serializing and deserializing `PublicKey`.
/// It uses a hex string for human-readable formats and raw bytes for binary formats.
mod pubkey_serde {
    use super::{PublicKey, VerifyingKey};
    use serde::{
        de::{self, Deserializer, Visitor},
        ser::Serializer,
    };
    use std::str::FromStr;

    pub fn serialize<S>(
        key: &VerifyingKey<super::Secp256k1>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            serializer.serialize_str(&PublicKey(*key).fingerprint())
        } else {
            serializer.serialize_bytes(key.to_encoded_point(true).as_bytes())
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<VerifyingKey<super::Secp256k1>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct KeyVisitor;

        impl<'de> Visitor<'de> for KeyVisitor {
            type Value = VerifyingKey<super::Secp256k1>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter
                    .write_str("a hex string or a byte array representing a compressed public key")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                PublicKey::from_str(v)
                    .map(|pk| pk.0)
                    .map_err(de::Error::custom)
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                VerifyingKey::from_sec1_bytes(v).map_err(de::Error::custom)
            }
        }

        if deserializer.is_human_readable() {
            deserializer.deserialize_str(KeyVisitor)
        } else {
            deserializer.deserialize_bytes(KeyVisitor)
        }
    }
}

/// A custom serde module for serializing and deserializing `Signature` as raw bytes.
pub mod signature_serde {
    use super::ECDSASignature;
    use k256::Secp256k1;
    use serde::{
        de::{self, Deserializer, Visitor},
        Serializer,
    };

    pub fn serialize<S>(sig: &ECDSASignature<Secp256k1>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&sig.to_bytes())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<ECDSASignature<Secp256k1>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SignatureVisitor;
        impl<'de> Visitor<'de> for SignatureVisitor {
            type Value = ECDSASignature<Secp256k1>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a 64-byte array representing an ECDSA signature")
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                ECDSASignature::from_bytes(v.into()).map_err(de::Error::custom)
            }
        }
        deserializer.deserialize_bytes(SignatureVisitor)
    }
}

/// A custom serde module for serializing and deserializing `ECDSASignature` via a hex string.
/// This is designed to be used with `serde_with::DisplayFromStr`.
pub mod serde_display_from_str {
    use super::ECDSASignature;
    use k256::Secp256k1;
    use serde::{Deserialize, Serialize};
    use std::{fmt, str::FromStr};

    /// A newtype wrapper around `ECDSASignature` to implement `Display` and `FromStr`
    /// for `serde_with`, satisfying the orphan rule.
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(transparent)]
    pub struct SignatureHex(pub ECDSASignature<Secp256k1>);

    impl From<ECDSASignature<Secp256k1>> for SignatureHex {
        fn from(sig: ECDSASignature<Secp256k1>) -> Self {
            Self(sig)
        }
    }

    impl From<SignatureHex> for ECDSASignature<Secp256k1> {
        fn from(sig_hex: SignatureHex) -> Self {
            sig_hex.0
        }
    }

    /// Implements `Display` for `SignatureHex`, serializing it to a hex string.
    impl fmt::Display for SignatureHex {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", hex::encode(self.0.to_bytes()))
        }
    }

    /// Implements `FromStr` for `SignatureHex`, deserializing it from a hex string.
    impl FromStr for SignatureHex {
        type Err = anyhow::Error;

        fn from_str(s: &str) -> Result<Self, Self::Err> {
            let bytes = hex::decode(s)
                .map_err(|e| anyhow::anyhow!("Failed to decode hex string for signature: {}", e))?;

            if bytes.len() != 64 {
                return Err(anyhow::anyhow!(
                    "Invalid signature length: expected 64 bytes, got {}",
                    bytes.len()
                ));
            }

            ECDSASignature::from_slice(&bytes)
                .map(SignatureHex)
                .map_err(|e| anyhow::anyhow!("Failed to create signature from bytes: {}", e))
        }
    }
}

/// A custom serde module for serializing and deserializing `ECDSASignature`.
/// It serializes to and from raw bytes for all formats, ensuring consistency
/// between hashing, network transmission (bincode), and storage.
pub mod serde_signature_bytes {
    use super::ECDSASignature;
    use k256::Secp256k1;
    use serde::{
        de::{self, Deserializer},
        Serializer,
    };
    use serde_bytes;

    pub fn serialize<S>(
        sig: &Option<ECDSASignature<Secp256k1>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(
            &sig.as_ref()
                .map(|s| s.to_bytes().to_vec())
                .unwrap_or_default(),
        )
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<Option<ECDSASignature<Secp256k1>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes: Vec<u8> = serde_bytes::deserialize(deserializer)?;
        if bytes.is_empty() {
            Ok(None)
        } else {
            ECDSASignature::from_slice(&bytes)
                .map(Some)
                .map_err(de::Error::custom)
        }
    }
}
