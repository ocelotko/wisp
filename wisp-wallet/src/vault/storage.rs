use crate::{
    common::constants::{ENCRYPTION_NONCE_SIZE, SALT_SIZE, WALLET_DIR, WALLET_FILE_EXTENSION},
    vault::crypto,
};
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Context, Result};
use rand::{rngs::OsRng, TryRngCore};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;
use wisp_core::address::Address;
use wisp_core::sha256::Hash;
use wisp_core::signatures::PublicKey;
use wisp_core::transactions::Script;

#[derive(Serialize, Deserialize, Clone)]
pub struct SavedWallet {
    pub name: String,
    pub encrypted_private_key: String,
    pub public_key: PublicKey,
    pub salt: Vec<u8>,
    #[serde(default)]
    pub encrypted_seed_phrase: Option<String>,
    #[serde(default)]
    pub derived_public_keys: Vec<PublicKey>,
    #[serde(default)]
    pub script_hashes: HashSet<Hash>,
}

impl SavedWallet {
    pub fn get_file_path(name: &str) -> PathBuf {
        let mut path = PathBuf::from(WALLET_DIR);
        path.push(format!("{}.{}", name, WALLET_FILE_EXTENSION));
        path
    }

    pub fn save_to_file(&self, password: &str) -> Result<()> {
        let path = Self::get_file_path(&self.name);
        fs::create_dir_all(WALLET_DIR)?;

        let serialized = serde_json::to_vec(self).context("Serialization failed")?;

        let mut nonce_bytes = [0u8; ENCRYPTION_NONCE_SIZE];
        OsRng.try_fill_bytes(&mut nonce_bytes)?;
        let nonce = Nonce::from(nonce_bytes);

        let key = crypto::derive_key(password, &self.salt)?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| anyhow!("Cipher error"))?;

        let ciphertext = cipher
            .encrypt(&nonce, serialized.as_slice())
            .map_err(|_| anyhow!("Encryption failed"))?;

        let mut file = File::create(&path)?;
        file.write_all(&nonce_bytes)?;
        file.write_all(&self.salt)?;
        file.write_all(&ciphertext)?;

        Ok(())
    }

    pub fn load_from_file(name: &str, password: &str) -> Result<Self> {
        let path = Self::get_file_path(name);
        let mut file = File::open(&path).context("Wallet file not found")?;

        let mut nonce_bytes = [0u8; ENCRYPTION_NONCE_SIZE];
        file.read_exact(&mut nonce_bytes)?;

        let mut salt = vec![0u8; SALT_SIZE];
        file.read_exact(&mut salt)?;

        let mut ciphertext = Vec::new();
        file.read_to_end(&mut ciphertext)?;

        let key = crypto::derive_key(password, &salt)?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| anyhow!("Cipher error"))?;

        let decrypted = cipher
            .decrypt(&Nonce::from(nonce_bytes), ciphertext.as_slice())
            .map_err(|_| anyhow!("Decryption failed (incorrect password)"))?;

        Ok(serde_json::from_slice(&decrypted)?)
    }

    pub fn is_script_relevant(&self, script: &Script) -> bool {
        self.derived_public_keys.iter().any(|pk| {
            let pk_hash = Address::hash160(pk);
            script.is_relevant_to(pk, &pk_hash, &self.script_hashes)
        })
    }
}
