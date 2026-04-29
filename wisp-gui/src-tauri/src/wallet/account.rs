use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose, Engine};
use bip39::Mnemonic;
use k256::ecdsa::SigningKey;
use log::info;
use rand::rngs::OsRng;
use rand::TryRngCore;

use wisp_core::{
    address::Address,
    sha256::{hash, Hash},
    signatures::{PrivateKey, PublicKey},
    transactions::Script,
};

use crate::wallet::{constants::*, core::Core, network::fetch_wallet_state, storage::SavedWallet};

pub fn derive_key_from_mnemonic(mnemonic: &Mnemonic, index: u32) -> Result<PrivateKey> {
    let seed = mnemonic.to_seed("");
    let mut data = seed.to_vec();
    data.extend_from_slice(&index.to_le_bytes());

    let signing_key = SigningKey::from_slice(&hash(&data).as_bytes())
        .map_err(|e| anyhow!("Failed to derive signing key: {}", e))?;
    Ok(PrivateKey(signing_key))
}

pub async fn create_wallet(
    core: &Core,
    name: &str,
    password: &str,
    config_path: &PathBuf,
) -> Result<String> {
    let mut wallets_guard = core.wallets.lock().await;
    if wallets_guard.iter().any(|w| w.name == name) {
        return Err(anyhow!("Wallet with name '{}' already exists", name));
    }

    let mut rng = OsRng;
    let mut entropy = [0u8; 32];
    rng.try_fill_bytes(&mut entropy)
        .map_err(|e| anyhow!("Failed to generate entropy: {}", e))?;
    let mnemonic = Mnemonic::from_entropy(&entropy)
        .map_err(|e| anyhow!("Failed to generate mnemonic: {}", e))?;
    let phrase = mnemonic.to_string();

    let private_key = derive_key_from_mnemonic(&mnemonic, 0)?;
    let public_key = private_key.public_key();

    let mut local_rng = OsRng;
    let mut salt = vec![0u8; SALT_SIZE];
    local_rng
        .try_fill_bytes(&mut salt)
        .map_err(|e| anyhow!("Failed to fill bytes for salt: {}", e))?;

    let hash_160 = Address::hash160(&public_key);
    let mut h_bytes = [0u8; 32];
    h_bytes[..20].copy_from_slice(&hash_160);
    let mut script_hashes = HashSet::new();
    script_hashes.insert(Hash::from_bytes(&h_bytes));

    let encrypted_private_key = encrypt_private_key(&private_key, password, &salt)?;
    let encrypted_seed_phrase = Some(encrypt_data(phrase.as_bytes(), password, &salt)?);

    let new_wallet = SavedWallet {
        name: name.to_string(),
        encrypted_private_key,
        public_key,
        salt,
        encrypted_seed_phrase,
        derived_public_keys: vec![public_key],
        script_hashes,
    };

    new_wallet.save_to_file(&core.data_dir, password)?;
    {
        let mut config_guard = core.config.lock().await;
        config_guard.current_wallet_name = Some(name.to_string());
        core.save_config(config_path, &*config_guard).await?;
    }

    let loaded_wallet = SavedWallet::load_from_file(&core.data_dir, name, password)?;
    wallets_guard.push(loaded_wallet);

    info!("Wallet '{}' created successfully!", name);
    Ok(phrase)
}

pub async fn recover_wallet_with_key(
    core: &Core,
    name: &str,
    password: &str,
    private_key_hex: &str,
    config_path: &PathBuf,
) -> Result<()> {
    let mut wallets_guard = core.wallets.lock().await;
    if wallets_guard.iter().any(|w| w.name == name) {
        return Err(anyhow!("Wallet with name '{}' already exists", name));
    }

    let private_key_bytes =
        hex::decode(private_key_hex).context("Invalid private key hex format")?;
    let signing_key = SigningKey::from_slice(&private_key_bytes)
        .map_err(|e| anyhow!("Invalid private key bytes: {}", e))?;
    let private_key = PrivateKey(signing_key);
    let public_key = private_key.public_key();

    let hash_160 = Address::hash160(&public_key);
    let mut h_bytes = [0u8; 32];
    h_bytes[..20].copy_from_slice(&hash_160);
    let mut script_hashes = HashSet::new();
    script_hashes.insert(Hash::from_bytes(&h_bytes));

    let mut rng = OsRng;
    let mut salt = vec![0u8; SALT_SIZE];
    rng.try_fill_bytes(&mut salt)
        .map_err(|e| anyhow!("Failed to fill bytes for salt: {}", e))?;

    let encrypted_private_key = encrypt_private_key(&private_key, password, &salt)?;

    let new_wallet = SavedWallet {
        name: name.to_string(),
        encrypted_private_key,
        public_key,
        salt,
        encrypted_seed_phrase: None,
        derived_public_keys: vec![public_key],
        script_hashes,
    };

    new_wallet.save_to_file(&core.data_dir, password)?;
    {
        let mut config_guard = core.config.lock().await;
        config_guard.current_wallet_name = Some(name.to_string());
        core.save_config(config_path, &*config_guard).await?;
    }

    let loaded_wallet = SavedWallet::load_from_file(&core.data_dir, name, password)?;
    wallets_guard.push(loaded_wallet);

    info!("Wallet '{}' recovered successfully!", name);
    Ok(())
}

pub async fn recover_wallet_with_seed(
    core: &Core,
    name: &str,
    password: &str,
    seed_phrase: &str,
    config_path: &PathBuf,
) -> Result<()> {
    let mut wallets_guard = core.wallets.lock().await;
    if wallets_guard.iter().any(|w| w.name == name) {
        return Err(anyhow!("Wallet with name '{}' already exists", name));
    }

    let mnemonic =
        Mnemonic::parse(seed_phrase).map_err(|e| anyhow!("Invalid seed phrase: {}", e))?;

    let private_key = derive_key_from_mnemonic(&mnemonic, 0)?;
    let public_key = private_key.public_key();

    let hash_160 = Address::hash160(&public_key);
    let mut h_bytes = [0u8; 32];
    h_bytes[..20].copy_from_slice(&hash_160);
    let mut script_hashes = HashSet::new();
    script_hashes.insert(Hash::from_bytes(&h_bytes));

    let mut rng = OsRng;
    let mut salt = vec![0u8; SALT_SIZE];
    rng.try_fill_bytes(&mut salt)
        .map_err(|e| anyhow!("Failed to fill bytes for salt: {}", e))?;

    let encrypted_private_key = encrypt_private_key(&private_key, password, &salt)?;
    let encrypted_seed_phrase = Some(encrypt_data(seed_phrase.as_bytes(), password, &salt)?);

    let new_wallet = SavedWallet {
        name: name.to_string(),
        encrypted_private_key,
        public_key,
        salt,
        encrypted_seed_phrase,
        derived_public_keys: vec![public_key],
        script_hashes,
    };

    new_wallet.save_to_file(&core.data_dir, password)?;
    {
        let mut config_guard = core.config.lock().await;
        config_guard.current_wallet_name = Some(name.to_string());
        core.save_config(config_path, &*config_guard).await?;
    }

    let loaded_wallet = SavedWallet::load_from_file(&core.data_dir, name, password)?;
    wallets_guard.push(loaded_wallet);

    info!("Wallet '{}' recovered from seed successfully!", name);
    Ok(())
}

pub async fn load_wallets() -> Result<Vec<String>> {
    let wallet_dir = PathBuf::from(WALLET_DIR);
    fs::create_dir_all(&wallet_dir)?;
    let mut wallet_names = Vec::new();
    for entry in fs::read_dir(&wallet_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            if let Some(name) = entry.file_name().to_str() {
                if let Some(base_name) = name.strip_suffix(&format!(".{}", WALLET_FILE_EXTENSION)) {
                    wallet_names.push(base_name.to_string());
                }
            }
        }
    }
    Ok(wallet_names)
}

pub async fn load_wallet(
    core: &Core,
    name: &str,
    password: &str,
    config_path: &PathBuf,
) -> Result<()> {
    info!("Loading wallet: {}", name);

    // Decrypt wallet file. This is the main blocking operation and also serves as password validation.
    let data_dir_clone = core.data_dir.clone();
    let name_clone = name.to_string();
    let password_clone = password.to_string();
    let loaded_wallet = tokio::task::spawn_blocking(move || {
        SavedWallet::load_from_file(&data_dir_clone, &name_clone, &password_clone)
    })
    .await
    .context("Failed to await wallet loading blocking task completion")?
    .context("Wallet loading blocking task returned an error")?;

    info!(
        "Wallet '{}' decrypted successfully (blocking task completed).",
        name
    );

    // Ensure all derived keys have their hashes in the watch-list
    let mut wallet = loaded_wallet;
    let mut updated = false;
    for pk in &wallet.derived_public_keys {
        let hash_160 = Address::hash160(pk);
        let mut h_bytes = [0u8; 32];
        h_bytes[..20].copy_from_slice(&hash_160);
        let h = Hash::from_bytes(&h_bytes);
        if wallet.script_hashes.insert(h) {
            updated = true;
        }
    }

    if updated {
        let wallet_to_save = wallet.clone();
        let data_dir_clone = core.data_dir.clone();
        let password_clone = password.to_string();
        tokio::task::spawn_blocking(move || {
            wallet_to_save.save_to_file(&data_dir_clone, &password_clone)
        })
        .await??;
    }

    // --- Atomic State Switch ---
    // Clear old wallet state before doing anything else. This prevents the UI from showing
    // stale data from the previous wallet. Any queries between now and when the new state
    // is fetched will return an empty/zero state, which is correct behavior during a switch.
    core.utxos.write().await.clear();
    core.transactions.write().await.clear();
    info!("Cleared wallet state (UTXOs and transactions) for wallet switch.");

    let mut wallets_guard = core.wallets.lock().await;
    if !wallets_guard.iter().any(|w| w.name == name) {
        wallets_guard.push(wallet);
    }
    drop(wallets_guard);

    {
        let mut config_guard = core.config.lock().await;
        config_guard.current_wallet_name = Some(name.to_string());
        core.save_config(config_path, &*config_guard).await?;
    }

    fetch_wallet_state(core).await?;
    info!("Wallet '{}' loaded and state fetched.", name);
    Ok(())
}

pub async fn get_current_wallet(core: &Core) -> Result<SavedWallet> {
    let config_guard = core.config.lock().await;
    match &config_guard.current_wallet_name {
        Some(name) => {
            let wallets_guard = core.wallets.lock().await;
            wallets_guard
                .iter()
                .find(|w| w.name == *name)
                .cloned()
                .ok_or_else(|| anyhow!("Current wallet '{}' not found in loaded wallets", name))
        }
        None => Err(anyhow!("No wallet loaded")),
    }
}

pub async fn decrypt_current_wallet_private_key(core: &Core, password: &str) -> Result<PrivateKey> {
    let current_wallet = get_current_wallet(core).await?;
    decrypt_private_key(
        &current_wallet.encrypted_private_key,
        password,
        &current_wallet.salt,
    )
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

pub fn encrypt_data(data: &[u8], password: &str, salt: &[u8]) -> Result<String> {
    let mut rng = OsRng;
    let mut nonce = [0u8; ENCRYPTION_NONCE_SIZE];
    rng.try_fill_bytes(&mut nonce)
        .map_err(|e| anyhow!("Failed to fill bytes for nonce: {}", e))?;
    let nonce = Nonce::from(nonce);

    let key = SavedWallet::derive_key(password, salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key).expect("key is correct length");

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

    let key = SavedWallet::derive_key(password, salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key).expect("key is correct length");

    cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|_| anyhow!("Incorrect password or corrupted data."))
}

pub async fn change_wallet_password(
    core: &Core,
    current_password: &str,
    new_password: &str,
) -> Result<()> {
    let wallet_data = get_current_wallet(core).await?;

    let private_key = decrypt_private_key(
        &wallet_data.encrypted_private_key,
        current_password,
        &wallet_data.salt,
    )
    .context("Incorrect current password or decryption failed")?;

    let mut wallets_guard = core.wallets.lock().await;

    if let Some(wallet_to_update) = wallets_guard
        .iter_mut()
        .find(|w| w.name == wallet_data.name)
    {
        let mut rng = OsRng;
        let mut new_salt = vec![0u8; SALT_SIZE];
        rng.try_fill_bytes(&mut new_salt)
            .map_err(|e| anyhow!("Failed to fill bytes for new salt: {}", e))?;

        wallet_to_update.salt = new_salt.clone();
        wallet_to_update.encrypted_private_key =
            encrypt_private_key(&private_key, new_password, &new_salt)?;

        if let Some(old_enc_seed) = &wallet_data.encrypted_seed_phrase {
            let seed_bytes = decrypt_data(old_enc_seed, current_password, &wallet_data.salt)?;
            let new_enc_seed = encrypt_data(&seed_bytes, new_password, &new_salt)?;
            wallet_to_update.encrypted_seed_phrase = Some(new_enc_seed);
        } else {
            wallet_to_update.encrypted_seed_phrase = None;
        }

        let wallet_clone = wallet_to_update.clone();
        let new_password_clone = new_password.to_string();
        let data_dir_clone = core.data_dir.clone();
        tokio::task::spawn_blocking(move || {
            wallet_clone.save_to_file(&data_dir_clone, &new_password_clone)
        })
        .await
        .context("Failed to await wallet save blocking task completion")?
        .context("Wallet save blocking task returned an error")?;

        info!(
            "Wallet password for '{}' changed successfully.",
            wallet_to_update.name
        );
        println!("Wallet password changed successfully.");
    } else {
        return Err(anyhow!(
            "Current wallet disappeared from memory during password change."
        ));
    }
    Ok(())
}

pub async fn export_seed_phrase(core: &Core, password: &str) -> Result<String> {
    let wallet = get_current_wallet(core).await?;
    if let Some(enc_seed) = &wallet.encrypted_seed_phrase {
        let seed_bytes = decrypt_data(enc_seed, password, &wallet.salt)?;
        let seed_str = String::from_utf8(seed_bytes).context("Invalid UTF-8 in seed phrase")?;
        Ok(seed_str)
    } else {
        Err(anyhow!("This wallet does not have a stored seed phrase (it might have been imported from a raw key)."))
    }
}

pub async fn delete_wallet(core: &Core, name: &str, config_path: &PathBuf) -> Result<()> {
    let path = SavedWallet::wallet_file_path(&core.data_dir, name);
    if fs::remove_file(&path).is_ok() {
        info!("Wallet file '{}' deleted.", name);
        {
            let mut config_guard = core.config.lock().await;
            if config_guard.current_wallet_name.as_deref() == Some(name) {
                config_guard.current_wallet_name = None;
                core.save_config(config_path, &*config_guard).await?;

                info!("Removed '{}' as the current wallet.", name);
            }
        }
        let mut wallets_guard = core.wallets.lock().await;
        wallets_guard.retain(|w| w.name != name);
        Ok(())
    } else {
        Err(anyhow!("Failed to delete wallet file '{}'", name))
    }
}
