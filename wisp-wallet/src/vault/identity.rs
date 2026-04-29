use anyhow::{anyhow, Context, Result};
use bip39::Mnemonic;
use k256::ecdsa::SigningKey;
use rand::rngs::OsRng;
use rand::TryRngCore;
use wisp_core::sha256::hash;
use wisp_core::signatures::PrivateKey;

pub struct IdentityManager;

impl IdentityManager {
    /// Generates a new 24-word mnemonic and the corresponding primary private key.
    pub fn generate_new_identity() -> Result<(String, PrivateKey)> {
        let mut entropy = [0u8; 32];
        OsRng
            .try_fill_bytes(&mut entropy)
            .map_err(|e| anyhow!("Failed to generate entropy: {}", e))?;

        let mnemonic = Mnemonic::from_entropy(&entropy)
            .map_err(|e| anyhow!("Failed to generate mnemonic: {}", e))?;
        let phrase = mnemonic.to_string();

        let private_key = Self::derive_key_from_phrase(&phrase, 0)?;
        Ok((phrase, private_key))
    }

    /// Derives the primary private key from a BIP39 seed phrase.
    /// Note: This follows your current hashing pattern. For a production protocol,
    /// you might later migrate this to a standard BIP32/BIP44 derivation path.
    pub fn derive_key_from_phrase(phrase: &str, index: u32) -> Result<PrivateKey> {
        let mnemonic = Mnemonic::parse(phrase).map_err(|e| anyhow!("Invalid mnemonic: {}", e))?;
        let seed = mnemonic.to_seed("");

        let mut data = seed.to_vec();
        data.extend_from_slice(&index.to_le_bytes());

        // Hash seed + index to get unique entropy per address
        let signing_key = SigningKey::from_slice(&hash(&data).as_bytes())
            .map_err(|e| anyhow!("Failed to derive signing key: {}", e))?;
        Ok(PrivateKey(signing_key))
    }

    /// Validates and imports an identity from a raw hex private key.
    pub fn from_raw_key(key_hex: &str) -> Result<PrivateKey> {
        let bytes = hex::decode(key_hex).context("Invalid hex format")?;
        let signing_key = SigningKey::from_slice(&bytes)
            .map_err(|e| anyhow!("Invalid private key bytes: {}", e))?;
        Ok(PrivateKey(signing_key))
    }
}
