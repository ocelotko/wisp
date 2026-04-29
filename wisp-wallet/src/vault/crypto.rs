use crate::common::constants::ENCRYPTION_NONCE_SIZE;
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Result};
use argon2::{
    password_hash::Result as PwHashResult, password_hash::Salt, Algorithm, Argon2, ParamsBuilder,
    PasswordHasher, Version,
};
use base64::{engine::general_purpose, Engine};
use k256::ecdsa::SigningKey;
use log::info;
use rand::{rngs::OsRng, TryRngCore};
use wisp_core::signatures::PrivateKey;

pub fn encrypt_data(data: &[u8], password: &str, salt: &[u8]) -> Result<String> {
    let mut rng = OsRng;
    let mut nonce = [0u8; ENCRYPTION_NONCE_SIZE];
    rng.try_fill_bytes(&mut nonce)
        .map_err(|e| anyhow!("Failed to fill bytes for nonce: {}", e))?;
    let nonce = Nonce::from(nonce);

    let key = derive_key(password, salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| anyhow!("Cipher error"))?; // Use map_err for consistency

    let ciphertext = cipher
        .encrypt(&nonce, data)
        .map_err(|e| anyhow!("Encryption failed: {}", e))?;

    let combined = [&nonce[..], &ciphertext[..]].concat();
    Ok(general_purpose::STANDARD_NO_PAD.encode(&combined))
}

pub fn decrypt_data(encrypted_data: &str, password: &str, salt: &[u8]) -> Result<Vec<u8>> {
    let decoded = general_purpose::STANDARD_NO_PAD.decode(encrypted_data)?;
    if decoded.len() <= ENCRYPTION_NONCE_SIZE {
        return Err(anyhow!("Invalid encrypted data format"));
    }
    let nonce_array: [u8; ENCRYPTION_NONCE_SIZE] = decoded[..ENCRYPTION_NONCE_SIZE]
        .try_into()
        .expect("Nonce slice has incorrect length");
    let nonce = Nonce::from(nonce_array);
    let ciphertext = &decoded[ENCRYPTION_NONCE_SIZE..];

    let key = derive_key(password, salt)?; // Call local derive_key
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| anyhow!("Cipher error"))?; // Use map_err for consistency

    cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|_| anyhow!("Incorrect password or corrupted data."))
}

pub fn encrypt_private_key(
    private_key: &PrivateKey,
    password: &str,
    salt: &[u8],
) -> Result<String> {
    encrypt_data(&private_key.0.to_bytes(), password, salt)
}

pub fn decrypt_private_key(
    encrypted_private_key: &str,
    password: &str,
    salt: &[u8],
) -> Result<PrivateKey> {
    let decrypted_bytes = decrypt_data(encrypted_private_key, password, salt)?;
    SigningKey::from_slice(&decrypted_bytes)
        .map(PrivateKey)
        .map_err(|_| anyhow!("Incorrect password or corrupted wallet file."))
}

pub fn derive_key(password: &str, salt_bytes: &[u8]) -> Result<Vec<u8>> {
    let params = ParamsBuilder::new()
        .t_cost(3)
        .m_cost(65536)
        .p_cost(4)
        .build()
        .map_err(|_| anyhow::anyhow!("Failed to build Argon2 parameters"))?;

    info!(
        "Derive Key - Raw Salt Bytes (Hex): {}",
        hex::encode(salt_bytes)
    );
    let salt_str = general_purpose::STANDARD_NO_PAD.encode(salt_bytes);
    info!("Derive Key - Base64 Encoded Salt: '{}'", salt_str);
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
