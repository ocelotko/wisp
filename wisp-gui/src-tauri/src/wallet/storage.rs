use std::{
    fs::{self, File},
    io::{Read, Write},
    path::PathBuf,
};

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::Result;
use argon2::password_hash::Result as PwHashResult;
use argon2::{password_hash::Salt, Algorithm, Argon2, ParamsBuilder, PasswordHasher, Version};
use base64::{engine::general_purpose, Engine as Base64Engine};
use rand::{rngs::OsRng, TryRngCore};
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashSet;

use crate::wallet::constants::*;
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
    pub fn wallet_file_path(data_dir: &PathBuf, name: &str) -> PathBuf {
        let mut path = data_dir.clone();
        path.push(WALLET_DIR);
        path.push(format!("{}.{}", name, WALLET_FILE_EXTENSION));
        path
    }

    pub fn save_to_file(&self, data_dir: &PathBuf, password: &str) -> Result<()> {
        let path = Self::wallet_file_path(data_dir, &self.name);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let serialized_result = serde_json::to_vec(self);
        match serialized_result {
            Ok(serialized_wallet) => {
                let mut rng = OsRng;
                let mut nonce = [0u8; ENCRYPTION_NONCE_SIZE];
                if let Err(e) = rng.try_fill_bytes(&mut nonce) {
                    return Err(anyhow::anyhow!("Failed to generate nonce: {:?}", e));
                }
                let nonce = Nonce::from(nonce);

                let key_result = Self::derive_key(password, &self.salt);
                match key_result {
                    Ok(key) => {
                        let cipher =
                            Aes256Gcm::new_from_slice(&key).expect("key is correct length");

                        let encrypted_result = cipher.encrypt(&nonce, serialized_wallet.as_slice());
                        match encrypted_result {
                            Ok(ciphertext) => {
                                let mut file = File::create(&path)?;
                                if let Err(e) = file.write_all(&nonce) {
                                    return Err(anyhow::anyhow!("Failed to write nonce: {:?}", e));
                                }
                                if let Err(e) = file.write_all(&self.salt) {
                                    return Err(anyhow::anyhow!("Failed to write salt: {:?}", e));
                                }
                                if let Err(e) = file.write_all(&ciphertext) {
                                    return Err(anyhow::anyhow!(
                                        "Failed to write ciphertext: {:?}",
                                        e
                                    ));
                                }
                                Ok(())
                            }
                            Err(e) => Err(anyhow::anyhow!("Encryption failed: {:?}", e)),
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(anyhow::anyhow!("Serialization failed: {:?}", e)),
        }
    }

    pub fn load_from_file(data_dir: &PathBuf, name: &str, password: &str) -> Result<Self> {
        let path = Self::wallet_file_path(data_dir, name);
        let mut file = File::open(&path)?;
        let mut nonce_bytes = [0u8; ENCRYPTION_NONCE_SIZE];
        file.read_exact(&mut nonce_bytes)?;
        let nonce = Nonce::from(nonce_bytes);

        let mut salt_bytes = vec![0u8; SALT_SIZE];
        file.read_exact(&mut salt_bytes)?;

        let key = Self::derive_key(password, &salt_bytes)?;
        let cipher = Aes256Gcm::new_from_slice(&key).expect("key is correct length");

        let mut ciphertext = Vec::new();
        file.read_to_end(&mut ciphertext)?;

        let decrypted_wallet = cipher
            .decrypt(&nonce, ciphertext.as_slice())
            .map_err(|e| anyhow::anyhow!("Decryption failed: {:?}", e))?;

        let wallet: SavedWallet = serde_json::from_slice(&decrypted_wallet)?;
        Ok(wallet)
    }

    pub fn derive_key(password: &str, salt_bytes: &[u8]) -> Result<Vec<u8>> {
        let params = ParamsBuilder::new()
            .t_cost(1)
            .m_cost(65536)
            .p_cost(1)
            .build()
            .map_err(|_| anyhow::anyhow!("Failed to build Argon2 parameters"))?;

        let salt_str = general_purpose::STANDARD_NO_PAD.encode(salt_bytes);
        let salt: PwHashResult<Salt> = Salt::from_b64(&salt_str);
        let salt = salt.map_err(|e| anyhow::anyhow!("Failed to create Salt from Base64: {}", e))?;

        let argon2 = Argon2::new(Algorithm::default(), Version::V0x13, params);

        let password_hash = argon2
            .hash_password(password.as_bytes(), salt)
            .map_err(|_| anyhow::anyhow!("Key derivation failed"))?;

        Ok(password_hash
            .hash
            .ok_or_else(|| anyhow::anyhow!("Password hash did not contain a hash"))?
            .as_bytes()
            .to_vec())
    }
}
