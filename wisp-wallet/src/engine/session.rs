use crate::common::config::Config;
use crate::common::constants::{SALT_SIZE, WALLET_DIR, WALLET_FILE_EXTENSION};
use crate::vault::crypto;
use crate::vault::identity::IdentityManager;
use crate::vault::storage::SavedWallet;
use anyhow::{anyhow, Context, Result};
use log::{debug, info};
use rand::rngs::OsRng;
use rand::TryRngCore;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::RwLock;
use wisp_core::address::Address;
use wisp_core::network::WalletTransactionInfo;
use wisp_core::sha256::Hash;
use wisp_core::signatures::PrivateKey;
use wisp_core::transactions::{OutPoint, Script, TransactionOutput};

pub struct Core {
    pub config: Arc<AsyncMutex<Config>>,
    pub wallets: Arc<AsyncMutex<Vec<SavedWallet>>>,
    // pub discovered_nodes: Arc<AsyncMutex<HashMap<String, Option<std::time::Duration>>>>,
    pub utxos: Arc<RwLock<HashMap<OutPoint, TransactionOutput>>>,
    pub transactions: Arc<RwLock<HashMap<Hash, WalletTransactionInfo>>>,
    pub connected_node_stream: Arc<AsyncMutex<Option<tokio::net::TcpStream>>>,
}

impl Core {
    pub async fn load(config_path: PathBuf) -> Result<Self> {
        let config = match fs::read_to_string(&config_path) {
            Ok(content) => toml::from_str(&content)?,
            Err(_) => {
                info!("No config file found, using default configuration.");
                Config::default()
            }
        };

        Ok(Core {
            config: Arc::new(AsyncMutex::new(config)),
            wallets: Arc::new(AsyncMutex::new(Vec::new())),
            // discovered_nodes: Arc::new(AsyncMutex::new(HashMap::new())),
            utxos: Arc::new(RwLock::new(HashMap::new())),
            transactions: Arc::new(RwLock::new(HashMap::new())),
            connected_node_stream: Arc::new(AsyncMutex::new(None)),
        })
    }

    pub async fn load_wallets() -> Result<Vec<String>> {
        let wallet_dir = PathBuf::from(WALLET_DIR);
        fs::create_dir_all(&wallet_dir)?;
        let mut wallet_names = Vec::new();
        for entry in fs::read_dir(&wallet_dir)? {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str() {
                if let Some(base_name) = name.strip_suffix(&format!(".{}", WALLET_FILE_EXTENSION)) {
                    wallet_names.push(base_name.to_string());
                }
            }
        }
        Ok(wallet_names)
    }

    pub async fn load_wallet(
        &self,
        name: &str,
        password: &str,
        config_path: &PathBuf,
    ) -> Result<()> {
        let name_clone = name.to_string();
        let password_clone = password.to_string();
        let loaded_wallet = tokio::task::spawn_blocking(move || {
            SavedWallet::load_from_file(&name_clone, &password_clone)
        })
        .await??;

        let mut updated = false;
        let mut wallet = loaded_wallet;

        // Ensure all derived keys have their hashes in the watch-list
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
            let wallet_clone = wallet.clone();
            let pwd_clone = password.to_string();
            tokio::task::spawn_blocking(move || wallet_clone.save_to_file(&pwd_clone)).await??;
        }

        {
            let mut wallets_guard = self.wallets.lock().await;
            if !wallets_guard.iter().any(|w| w.name == name) {
                wallets_guard.push(wallet);
            }
        }
        {
            let mut config_guard = self.config.lock().await;
            config_guard.current_wallet_name = Some(name.to_string());
            self.save_config(config_path, &*config_guard).await?;
        }

        self.fetch_wallet_state().await?;
        Ok(())
    }

    pub async fn create_wallet(
        &self,
        name: &str,
        password: &str,
        config_path: &PathBuf,
    ) -> Result<String> {
        let mut wallets_guard = self.wallets.lock().await;
        if wallets_guard.iter().any(|w| w.name == name) {
            return Err(anyhow!("Wallet '{}' already exists", name));
        }

        let (phrase, private_key) = IdentityManager::generate_new_identity()?;
        let public_key = private_key.public_key();

        let mut salt = vec![0u8; SALT_SIZE];
        OsRng.try_fill_bytes(&mut salt)?;

        let hash_160 = wisp_core::address::Address::hash160(&public_key);
        let mut h_bytes = [0u8; 32];
        h_bytes[..20].copy_from_slice(&hash_160);
        let primary_hash = Hash::from_bytes(&h_bytes);

        let encrypted_private_key = crypto::encrypt_private_key(&private_key, password, &salt)?;
        let encrypted_seed_phrase = Some(crypto::encrypt_data(phrase.as_bytes(), password, &salt)?);

        let mut script_hashes = HashSet::new();
        script_hashes.insert(primary_hash);

        let new_wallet = SavedWallet {
            name: name.to_string(),
            encrypted_private_key,
            public_key,
            salt,
            encrypted_seed_phrase,
            derived_public_keys: vec![public_key],
            script_hashes,
        };

        new_wallet.save_to_file(password)?;
        wallets_guard.push(SavedWallet::load_from_file(name, password)?);

        let mut config_guard = self.config.lock().await;
        config_guard.current_wallet_name = Some(name.to_string());
        self.save_config(config_path, &*config_guard).await?;

        Ok(phrase)
    }

    pub async fn recover_wallet_with_key(
        &self,
        name: &str,
        password: &str,
        key_hex: &str,
        config_path: &PathBuf,
    ) -> Result<()> {
        let private_key = IdentityManager::from_raw_key(key_hex)?;

        let mut salt = vec![0u8; SALT_SIZE];
        let public_key = private_key.public_key();

        let hash_160 = wisp_core::address::Address::hash160(&public_key);
        let mut h_bytes = [0u8; 32];
        h_bytes[..20].copy_from_slice(&hash_160);
        let primary_hash = Hash::from_bytes(&h_bytes);

        OsRng.try_fill_bytes(&mut salt)?;

        let mut script_hashes = HashSet::new();
        script_hashes.insert(primary_hash);

        let new_wallet = SavedWallet {
            name: name.to_string(),
            encrypted_private_key: crypto::encrypt_private_key(&private_key, password, &salt)?,
            public_key: private_key.public_key(),
            salt,
            encrypted_seed_phrase: None,
            derived_public_keys: vec![public_key],
            script_hashes,
        };

        new_wallet.save_to_file(password)?;
        self.wallets
            .lock()
            .await
            .push(SavedWallet::load_from_file(name, password)?);

        let mut config_guard = self.config.lock().await;
        config_guard.current_wallet_name = Some(name.to_string());
        self.save_config(config_path, &*config_guard).await?;

        Ok(())
    }

    pub async fn recover_wallet_with_seed(
        &self,
        name: &str,
        password: &str,
        phrase: &str,
        config_path: &PathBuf,
    ) -> Result<()> {
        let private_key = IdentityManager::derive_key_from_phrase(phrase, 0)?;
        let public_key = private_key.public_key();

        let mut salt = vec![0u8; SALT_SIZE];
        OsRng.try_fill_bytes(&mut salt)?;

        let hash_160 = wisp_core::address::Address::hash160(&public_key);
        let mut h_bytes = [0u8; 32];
        h_bytes[..20].copy_from_slice(&hash_160);
        let primary_hash = Hash::from_bytes(&h_bytes);

        let encrypted_private_key = crypto::encrypt_private_key(&private_key, password, &salt)?;
        let encrypted_seed_phrase = Some(crypto::encrypt_data(phrase.as_bytes(), password, &salt)?);

        let mut script_hashes = HashSet::new();
        script_hashes.insert(primary_hash);

        let new_wallet = SavedWallet {
            name: name.to_string(),
            encrypted_private_key,
            public_key,
            salt,
            encrypted_seed_phrase,
            derived_public_keys: vec![public_key],
            script_hashes,
        };

        new_wallet.save_to_file(password)?;
        self.wallets
            .lock()
            .await
            .push(SavedWallet::load_from_file(name, password)?);

        let mut config_guard = self.config.lock().await;
        config_guard.current_wallet_name = Some(name.to_string());
        self.save_config(config_path, &*config_guard).await?;

        Ok(())
    }

    pub async fn change_wallet_password(&self, current_pwd: &str, new_pwd: &str) -> Result<()> {
        let wallet_data = self.get_current_wallet().await?;
        let private_key = crypto::decrypt_private_key(
            &wallet_data.encrypted_private_key,
            current_pwd,
            &wallet_data.salt,
        )?;

        let mut wallets_guard = self.wallets.lock().await;
        let wallet = wallets_guard
            .iter_mut()
            .find(|w| w.name == wallet_data.name)
            .ok_or_else(|| anyhow!("Wallet disappeared from memory"))?;

        let mut new_salt = vec![0u8; SALT_SIZE];
        OsRng.try_fill_bytes(&mut new_salt)?;

        wallet.salt = new_salt.clone();
        wallet.encrypted_private_key =
            crypto::encrypt_private_key(&private_key, new_pwd, &new_salt)?;

        if let Some(old_enc_seed) = &wallet_data.encrypted_seed_phrase {
            let seed_bytes = crypto::decrypt_data(old_enc_seed, current_pwd, &wallet_data.salt)?;
            wallet.encrypted_seed_phrase =
                Some(crypto::encrypt_data(&seed_bytes, new_pwd, &new_salt)?);
        }

        let wallet_to_save = wallet.clone();
        let pwd_clone = new_pwd.to_string();
        tokio::task::spawn_blocking(move || wallet_to_save.save_to_file(&pwd_clone)).await??;

        Ok(())
    }

    pub async fn export_seed_phrase(&self, password: &str) -> Result<String> {
        let wallet = self.get_current_wallet().await?;
        let enc_seed = wallet
            .encrypted_seed_phrase
            .as_ref()
            .ok_or_else(|| anyhow!("No seed phrase stored (wallet likely imported via key)"))?;

        let seed_bytes = crypto::decrypt_data(enc_seed, password, &wallet.salt)?;
        String::from_utf8(seed_bytes).context("Invalid UTF-8 in seed")
    }

    pub async fn decrypt_current_private_key(&self, password: &str) -> Result<PrivateKey> {
        let wallet = self.get_current_wallet().await?;
        crypto::decrypt_private_key(&wallet.encrypted_private_key, password, &wallet.salt)
    }

    pub async fn get_current_wallet(&self) -> Result<SavedWallet> {
        let config_guard = self.config.lock().await;
        match &config_guard.current_wallet_name {
            Some(name) => {
                let wallets_guard = self.wallets.lock().await;
                wallets_guard
                    .iter()
                    .find(|w| w.name == *name)
                    .cloned()
                    .ok_or_else(|| anyhow!("Current wallet '{}' not found in loaded wallets", name))
            }
            None => Err(anyhow!("No wallet loaded")),
        }
    }

    pub async fn generate_new_address(
        &self,
        password: &str,
    ) -> Result<(u32, Vec<(String, String)>)> {
        let mut wallet = self.get_current_wallet().await?;
        let phrase_enc = wallet.encrypted_seed_phrase.as_ref().ok_or_else(|| {
            anyhow!("Wallet was imported without a seed phrase; cannot derive new addresses.")
        })?;

        let phrase_bytes = crypto::decrypt_data(phrase_enc, password, &wallet.salt)?;
        let phrase = String::from_utf8(phrase_bytes)?;

        let index = wallet.derived_public_keys.len() as u32;
        let new_pk = IdentityManager::derive_key_from_phrase(&phrase, index)?.public_key();

        let hash_160 = Address::hash160(&new_pk);
        let mut h_bytes = [0u8; 32];
        h_bytes[..20].copy_from_slice(&hash_160);
        let h = Hash::from_bytes(&h_bytes);

        wallet.derived_public_keys.push(new_pk);
        wallet.script_hashes.insert(h);

        let wallet_to_save = wallet.clone();
        let pwd_clone = password.to_string();
        tokio::task::spawn_blocking(move || wallet_to_save.save_to_file(&pwd_clone)).await??;

        // Update in-memory wallet list. We use a scope here to ensure the lock
        // is dropped before we call fetch_wallet_state (to avoid deadlock).
        {
            let mut wallets = self.wallets.lock().await;
            if let Some(w) = wallets.iter_mut().find(|w| w.name == wallet.name) {
                *w = wallet.clone();
            }
        }

        self.fetch_wallet_state().await?;
        let new_index = (wallet.derived_public_keys.len() - 1) as u32;

        // Return the formatted addresses for the newly generated key
        let formatted = self.format_addresses(&new_pk, new_index)?;
        Ok((new_index, formatted))
    }

    /// Logic for deriving and formatting receive addresses.
    pub fn get_receive_addresses(&self, wallet: &SavedWallet) -> Result<Vec<(String, String)>> {
        let mut list = Vec::new();
        for (i, pk) in wallet.derived_public_keys.iter().enumerate() {
            list.extend(self.format_addresses(pk, i as u32)?);
        }
        Ok(list)
    }

    fn format_addresses(
        &self,
        pk: &wisp_core::signatures::PublicKey,
        index: u32,
    ) -> Result<Vec<(String, String)>> {
        let hash_160 = Address::hash160(pk);
        let mut h_bytes = [0u8; 32];
        h_bytes[..20].copy_from_slice(&hash_160);
        let h = Hash::from_bytes(&h_bytes);

        Ok(vec![
            (
                format!("{:<25}", format!("Address #{} (Aurora):", index + 1)),
                Address::encode(&Script::Aurora(h)),
            ),
            (
                format!("{:<25}", format!("Address #{} (Shadow):", index + 1)),
                Address::encode(&Script::Shadow(h)),
            ),
            (
                format!("{:<25}", format!("Address #{} (Classic):", index + 1)),
                Address::encode(&Script::Classic(*pk)),
            ),
        ])
    }

    pub async fn save_config(&self, path: &PathBuf, config_data: &Config) -> Result<()> {
        debug!("save_config: Attempting to save config to path: {:?}", path);

        debug!("save_config: Serializing config to TOML.");
        let config_string =
            toml::to_string_pretty(config_data).context("Failed to serialize config to TOML")?;

        debug!("save_config: Writing config to file.");
        tokio::fs::write(path, config_string)
            .await
            .context("Failed to write config file")?;

        info!("Config saved to {:?}", path);
        Ok(())
    }

    pub async fn delete_wallet(&self, name: &str, config_path: &PathBuf) -> Result<()> {
        let path = SavedWallet::get_file_path(name);
        if fs::remove_file(&path).is_ok() {
            info!("Wallet file '{}' deleted.", name);
            {
                let mut config_guard = self.config.lock().await;
                if config_guard.current_wallet_name.as_deref() == Some(name) {
                    config_guard.current_wallet_name = None;
                    self.save_config(config_path, &*config_guard).await?;
                    info!("Removed '{}' as the current wallet.", name);
                }
            }
            let mut wallets_guard = self.wallets.lock().await;
            wallets_guard.retain(|w| w.name != name);
            Ok(())
        } else {
            Err(anyhow!("Failed to delete wallet file '{}'", name))
        }
    }

    pub async fn get_default_node_address(&self) -> String {
        let config_guard = self.config.lock().await;
        config_guard.default_node.clone()
    }

    pub async fn get_node_response_timeout(&self) -> std::time::Duration {
        let config_guard = self.config.lock().await;
        std::time::Duration::from_secs(config_guard.node_response_timeout_secs)
    }
}
