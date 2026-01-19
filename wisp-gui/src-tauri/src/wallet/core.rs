use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use std::{fs, sync::Arc};

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose, Engine};
use bip39::Mnemonic;
use chrono::Utc;
use k256::ecdsa::signature::Signer;
use k256::ecdsa::SigningKey;
use log::{debug, info, warn};
use rand::rngs::OsRng;
use rand::TryRngCore;
use tokio::sync::{Mutex as AsyncMutex, MutexGuard};
use tokio::time::timeout;
use tokio::{net::TcpStream, sync::RwLock};

use wisp_core::{
    blockchain::Block,
    currency::Amount,
    network::WalletTransactionInfo,
    network::{ChainMessage, Message, TransactionStatus, WalletMessage},
    sha256::{hash, Hash},
    signatures::{PrivateKey, PublicKey},
    transactions::{OutPoint, Transaction, TransactionInput, TransactionOutput},
};

use crate::wallet::{config::Config, constants::*, storage::SavedWallet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeeType {
    Fixed,
    Percent,
}

impl std::fmt::Display for FeeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeeType::Fixed => write!(f, "Fixed Amount"),
            FeeType::Percent => write!(f, "Percentage of Amount Sent"),
        }
    }
}

pub struct Core {
    pub config: Arc<AsyncMutex<Config>>,
    pub wallets: Arc<AsyncMutex<Vec<SavedWallet>>>,
    pub discovered_nodes: Arc<AsyncMutex<HashMap<String, Option<Duration>>>>,
    pub utxos: Arc<RwLock<HashMap<OutPoint, TransactionOutput>>>,
    pub transactions: Arc<RwLock<HashMap<Hash, WalletTransactionInfo>>>,
    connected_node_stream: Arc<AsyncMutex<Option<TcpStream>>>,
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
            discovered_nodes: Arc::new(AsyncMutex::new(HashMap::new())),
            utxos: Arc::new(RwLock::new(HashMap::new())),
            transactions: Arc::new(RwLock::new(HashMap::new())),
            connected_node_stream: Arc::new(AsyncMutex::new(None)),
        })
    }

    pub async fn set_default_node(
        &self,
        new_node_address: &str,
        config_path: &PathBuf,
    ) -> Result<()> {
        let mut config_guard = self.config.lock().await;
        config_guard.default_node = new_node_address.to_string();

        // Invalidate current connection so next call to get_connected_stream will use the new address
        let mut stream_lock = self.connected_node_stream.lock().await;
        *stream_lock = None;

        // The config_guard is passed by reference, so it's unlocked after the call.
        self.save_config(config_path, &*config_guard).await?;

        info!("Default node address updated to: {}", new_node_address);
        Ok(())
    }

    async fn get_default_node_address(&self) -> String {
        let config_guard = self.config.lock().await;
        config_guard.default_node.clone()
    }

    pub async fn get_node_response_timeout(&self) -> Duration {
        let config_guard = self.config.lock().await;
        Duration::from_secs(config_guard.node_response_timeout_secs)
    }

    pub async fn get_connected_stream(&self) -> Result<MutexGuard<'_, Option<TcpStream>>> {
        let mut stream_lock = self.connected_node_stream.lock().await;

        if stream_lock.is_none() {
            let node_address = self.get_default_node_address().await;
            let connect_timeout =
                Duration::from_secs(self.config.lock().await.node_connect_timeout_secs);

            info!("Attempting to connect to node at: {}", node_address);
            let stream = match timeout(connect_timeout, TcpStream::connect(&node_address)).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    return Err(anyhow!("Failed to connect to node {}: {}", node_address, e))
                }
                Err(e) => return Err(anyhow!("Connection timed out to {}: {}", node_address, e)),
            };

            if let Err(e) = stream.set_nodelay(true) {
                warn!("Failed to set nodelay on stream: {}", e);
            }

            info!("Successfully connected to node at {}", node_address);
            *stream_lock = Some(stream);
        } else {
            debug!("Re-using existing connection to node.");
        }

        Ok(stream_lock)
    }

    /// Saves the current configuration to a file.
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

    pub async fn create_wallet(
        &self,
        name: &str,
        password: &str,
        config_path: &PathBuf,
    ) -> Result<String> {
        let mut wallets_guard = self.wallets.lock().await;
        if wallets_guard.iter().any(|w| w.name == name) {
            return Err(anyhow!("Wallet with name '{}' already exists", name));
        }

        // Generate a new mnemonic (24 words)
        let mut rng = OsRng;
        let mut entropy = [0u8; 32];
        rng.try_fill_bytes(&mut entropy)
            .map_err(|e| anyhow!("Failed to generate entropy: {}", e))?;
        let mnemonic = Mnemonic::from_entropy(&entropy)
            .map_err(|e| anyhow!("Failed to generate mnemonic: {}", e))?;
        let phrase = mnemonic.to_string();

        let seed = mnemonic.to_seed("");
        let private_key_hash = hash(&seed[..]);
        let private_key_bytes = private_key_hash.as_bytes();

        let signing_key = SigningKey::from_slice(&private_key_bytes)
            .map_err(|e| anyhow!("Invalid private key derived from seed: {}", e))?;
        let private_key = PrivateKey(signing_key);
        let public_key = private_key.public_key();

        let mut local_rng = OsRng;
        let mut salt = vec![0u8; SALT_SIZE];
        local_rng
            .try_fill_bytes(&mut salt)
            .map_err(|e| anyhow!("Failed to fill bytes for salt: {}", e))?;

        let encrypted_private_key = Self::encrypt_private_key(&private_key, password, &salt)?;
        let encrypted_seed_phrase = Some(Self::encrypt_data(phrase.as_bytes(), password, &salt)?);

        let new_wallet = SavedWallet {
            name: name.to_string(),
            encrypted_private_key,
            public_key,
            salt,
            encrypted_seed_phrase,
        };

        new_wallet.save_to_file(password)?;

        {
            let mut config_guard = self.config.lock().await;
            config_guard.current_wallet_name = Some(name.to_string());
            self.save_config(config_path, &*config_guard).await?;
        }

        let loaded_wallet = SavedWallet::load_from_file(name, password)?;
        wallets_guard.push(loaded_wallet);

        info!("Wallet '{}' created successfully!", name);
        Ok(phrase)
    }

    pub async fn recover_wallet_with_key(
        &self,
        name: &str,
        password: &str,
        private_key_hex: &str,
        config_path: &PathBuf,
    ) -> Result<()> {
        let mut wallets_guard = self.wallets.lock().await;
        if wallets_guard.iter().any(|w| w.name == name) {
            return Err(anyhow!("Wallet with name '{}' already exists", name));
        }

        let private_key_bytes =
            hex::decode(private_key_hex).context("Invalid private key hex format")?;
        let signing_key = SigningKey::from_slice(&private_key_bytes)
            .map_err(|e| anyhow!("Invalid private key bytes: {}", e))?;
        let private_key = PrivateKey(signing_key);
        let public_key = private_key.public_key();

        let mut rng = OsRng;
        let mut salt = vec![0u8; SALT_SIZE];
        rng.try_fill_bytes(&mut salt)
            .map_err(|e| anyhow!("Failed to fill bytes for salt: {}", e))?;

        let encrypted_private_key = Self::encrypt_private_key(&private_key, password, &salt)?;

        let new_wallet = SavedWallet {
            name: name.to_string(),
            encrypted_private_key,
            public_key,
            salt,
            encrypted_seed_phrase: None,
        };

        new_wallet.save_to_file(password)?;

        {
            let mut config_guard = self.config.lock().await;
            config_guard.current_wallet_name = Some(name.to_string());
            self.save_config(config_path, &*config_guard).await?;
        }

        let loaded_wallet = SavedWallet::load_from_file(name, password)?;
        wallets_guard.push(loaded_wallet);

        info!("Wallet '{}' recovered successfully!", name);
        Ok(())
    }

    pub async fn recover_wallet_with_seed(
        &self,
        name: &str,
        password: &str,
        seed_phrase: &str,
        config_path: &PathBuf,
    ) -> Result<()> {
        let mut wallets_guard = self.wallets.lock().await;
        if wallets_guard.iter().any(|w| w.name == name) {
            return Err(anyhow!("Wallet with name '{}' already exists", name));
        }

        let mnemonic =
            Mnemonic::parse(seed_phrase).map_err(|e| anyhow!("Invalid seed phrase: {}", e))?;

        let seed = mnemonic.to_seed("");
        let private_key_hash = hash(&seed[..]);
        let private_key_bytes = private_key_hash.as_bytes();

        let signing_key = SigningKey::from_slice(&private_key_bytes)
            .map_err(|e| anyhow!("Invalid private key derived from seed: {}", e))?;
        let private_key = PrivateKey(signing_key);
        let public_key = private_key.public_key();

        let mut rng = OsRng;
        let mut salt = vec![0u8; SALT_SIZE];
        rng.try_fill_bytes(&mut salt)
            .map_err(|e| anyhow!("Failed to fill bytes for salt: {}", e))?;

        let encrypted_private_key = Self::encrypt_private_key(&private_key, password, &salt)?;
        let encrypted_seed_phrase =
            Some(Self::encrypt_data(seed_phrase.as_bytes(), password, &salt)?);

        let new_wallet = SavedWallet {
            name: name.to_string(),
            encrypted_private_key,
            public_key,
            salt,
            encrypted_seed_phrase,
        };

        new_wallet.save_to_file(password)?;

        {
            let mut config_guard = self.config.lock().await;
            config_guard.current_wallet_name = Some(name.to_string());
            self.save_config(config_path, &*config_guard).await?;
        }

        let loaded_wallet = SavedWallet::load_from_file(name, password)?;
        wallets_guard.push(loaded_wallet);

        info!("Wallet '{}' recovered from seed successfully!", name);
        Ok(())
    }

    pub async fn load_wallets() -> Result<Vec<String>> {
        let wallet_dir = PathBuf::from(WALLET_DIR);
        fs::create_dir_all(&wallet_dir)?; // Ensure directory exists
        let mut wallet_names = Vec::new();
        for entry in fs::read_dir(&wallet_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                if let Some(name) = entry.file_name().to_str() {
                    if let Some(base_name) =
                        name.strip_suffix(&format!(".{}", WALLET_FILE_EXTENSION))
                    {
                        wallet_names.push(base_name.to_string());
                    }
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
        info!("Loading wallet: {}", name);

        let name_clone = name.to_string();
        let password_clone = password.to_string();

        let loaded_wallet = tokio::task::spawn_blocking(move || {
            SavedWallet::load_from_file(&name_clone, &password_clone)
                .context("Failed to load wallet file in blocking task")
        })
        .await
        .context("Failed to await wallet loading blocking task completion")?
        .context("Wallet loading blocking task returned an error")?;

        info!(
            "Wallet '{}' decrypted successfully (blocking task completed).",
            name
        );

        let mut wallets_guard = self.wallets.lock().await;
        debug!("load_wallet: Acquired wallets_guard lock.");

        if wallets_guard.iter().any(|w| w.name == name) {
            info!("Wallet '{}' is already loaded.", name);
            drop(wallets_guard);

            let mut config_guard = self.config.lock().await;
            config_guard.current_wallet_name = Some(name.to_string());
            self.save_config(config_path, &*config_guard).await?;

            debug!("load_wallet: Exiting early because wallet is already loaded.");
            return Ok(());
        }

        debug!("load_wallet: Pushing loaded wallet to in-memory list.");
        wallets_guard.push(loaded_wallet.clone());
        drop(wallets_guard);
        debug!("load_wallet: Released wallets_guard lock.");

        debug!("load_wallet: Attempting to acquire config_guard lock.");
        {
            let mut config_guard = self.config.lock().await;
            config_guard.current_wallet_name = Some(name.to_string());

            debug!("load_wallet: Calling save_config.");
            self.save_config(config_path, &*config_guard).await?;
            debug!("load_wallet: Config updated and saved.");
        }

        debug!("load_wallet: Calling fetch_wallet_state...");
        self.fetch_wallet_state().await?;
        info!("Wallet '{}' loaded and state fetched.", name);
        Ok(())
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

    pub async fn decrypt_current_wallet_private_key(&self, password: &str) -> Result<PrivateKey> {
        let current_wallet = self.get_current_wallet().await?;
        Self::decrypt_private_key(
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
        Self::encrypt_data(&private_key.0.to_bytes(), password, salt)
    }

    fn decrypt_private_key(
        encrypted_private_key: &str,
        password: &str,
        salt: &[u8],
    ) -> Result<PrivateKey> {
        let decrypted_bytes = Self::decrypt_data(encrypted_private_key, password, salt)?;
        SigningKey::from_slice(&decrypted_bytes)
            .map(PrivateKey)
            .map_err(|_| anyhow!("Incorrect password or corrupted wallet file."))
    }

    fn encrypt_data(data: &[u8], password: &str, salt: &[u8]) -> Result<String> {
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

    fn decrypt_data(encrypted_data: &str, password: &str, salt: &[u8]) -> Result<Vec<u8>> {
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
        &self,
        current_password: &str,
        new_password: &str,
    ) -> Result<()> {
        let wallet_data = self.get_current_wallet().await?;

        let private_key = Core::decrypt_private_key(
            &wallet_data.encrypted_private_key,
            current_password,
            &wallet_data.salt,
        )
        .context("Incorrect current password or decryption failed")?;

        let mut wallets_guard = self.wallets.lock().await;

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
                Self::encrypt_private_key(&private_key, new_password, &new_salt)?;

            if let Some(old_enc_seed) = &wallet_data.encrypted_seed_phrase {
                let seed_bytes =
                    Self::decrypt_data(old_enc_seed, current_password, &wallet_data.salt)?;
                let new_enc_seed = Self::encrypt_data(&seed_bytes, new_password, &new_salt)?;
                wallet_to_update.encrypted_seed_phrase = Some(new_enc_seed);
            } else {
                wallet_to_update.encrypted_seed_phrase = None;
            }

            let wallet_clone = wallet_to_update.clone();
            let new_password_clone = new_password.to_string();
            tokio::task::spawn_blocking(move || wallet_clone.save_to_file(&new_password_clone))
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

    pub async fn export_seed_phrase(&self, password: &str) -> Result<String> {
        let wallet = self.get_current_wallet().await?;
        if let Some(enc_seed) = &wallet.encrypted_seed_phrase {
            let seed_bytes = Self::decrypt_data(enc_seed, password, &wallet.salt)?;
            let seed_str = String::from_utf8(seed_bytes).context("Invalid UTF-8 in seed phrase")?;
            Ok(seed_str)
        } else {
            Err(anyhow!("This wallet does not have a stored seed phrase (it might have been imported from a raw key)."))
        }
    }

    pub async fn delete_wallet(&self, name: &str, config_path: &PathBuf) -> Result<()> {
        let path = SavedWallet::wallet_file_path(name);
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

    pub async fn fetch_peers_from_node(&self) -> Result<Vec<String>> {
        let mut stream_guard = self.get_connected_stream().await?;
        let stream_ref = stream_guard
            .as_mut()
            .expect("Expected an active TCP stream after connection attempt");
        let response_timeout = self.get_node_response_timeout().await;

        let msg = Message::P2P(wisp_core::network::P2PMessage::DiscoverNodes);
        if let Err(e) = msg.send_async(stream_ref).await {
            *stream_guard = None;
            return Err(anyhow!("Failed to send DiscoverNodes message: {}", e));
        }

        let response = match tokio::time::timeout(
            response_timeout,
            Message::receive_async(stream_ref),
        )
        .await
        {
            Ok(Ok(msg)) => msg,
            Ok(Err(e)) => {
                *stream_guard = None;
                return Err(anyhow!("Failed to receive NodeList response: {}", e));
            }
            Err(_) => {
                *stream_guard = None;
                return Err(anyhow!("Timeout waiting for NodeList response"));
            }
        };

        match response {
            Message::P2P(wisp_core::network::P2PMessage::NodeList(peers)) => Ok(peers),
            other => {
                *stream_guard = None;
                Err(anyhow!(
                    "Unexpected response for DiscoverNodes: {:?}",
                    other
                ))
            }
        }
    }

    pub async fn fetch_wallet_state(&self) -> Result<()> {
        let current_wallet = self.get_current_wallet().await?;
        let wallet_public_key = current_wallet.public_key.clone();
        info!(
            "Fetching wallet state for public key: {}",
            wallet_public_key.fingerprint()
        );

        debug!("fetch_wallet_state: Attempting to get connected stream.");
        let mut stream_guard = self.get_connected_stream().await?;
        debug!("fetch_wallet_state: Connected stream obtained.");
        let stream_ref = stream_guard
            .as_mut()
            .expect("Expected an active TCP stream after connection attempt");
        let response_timeout = self.get_node_response_timeout().await;

        let fetch_state_msg =
            Message::Wallet(WalletMessage::FetchWalletState(wallet_public_key.clone()));
        if let Err(e) = fetch_state_msg.send_async(stream_ref).await {
            *stream_guard = None;
            return Err(anyhow!("Failed to send FetchWalletState message: {}", e));
        }
        debug!("fetch_wallet_state: Waiting for WalletState response.");

        let state_response = match tokio::time::timeout(
            response_timeout,
            Message::receive_async(stream_ref),
        )
        .await
        {
            Ok(Ok(msg)) => msg,
            Ok(Err(e)) => {
                *stream_guard = None;
                return Err(anyhow!("Failed to receive WalletState response: {}", e));
            }
            Err(_) => {
                *stream_guard = None;
                return Err(anyhow!("Timeout waiting for WalletState response"));
            }
        };

        match state_response {
            Message::Wallet(WalletMessage::WalletState(snapshot)) => {
                debug!("fetch_wallet_state: Received WalletState snapshot.");
                let mut transactions_guard = self.transactions.write().await;
                transactions_guard.clear();
                for tx_info in snapshot.transactions {
                    transactions_guard.insert(tx_info.transaction.txid()?, tx_info);
                }
                info!(
                    "Updated wallet with {} total transactions.",
                    transactions_guard.len()
                );

                let mut utxos_guard = self.utxos.write().await;
                utxos_guard.clear();
                for (outpoint, output) in snapshot.utxos {
                    utxos_guard.insert(outpoint, output);
                }

                info!("Fetched {} available UTXOs for wallet.", utxos_guard.len());
            }
            other => {
                *stream_guard = None;
                return Err(anyhow!(
                    "Unexpected response for FetchWalletState: {:?}",
                    other
                ));
            }
        }

        info!("fetch_wallet_state: Wallet state fetch completed successfully.");
        Ok(())
    }

    pub async fn get_total_balance(&self) -> Result<Amount> {
        let utxos_guard = self.utxos.read().await;
        let transactions_guard = self.transactions.read().await;
        let wallet_public_key = self.get_current_wallet().await?.public_key;
        let confirmed_balance: Amount = utxos_guard.values().map(|output| output.value).sum();

        let mut pending_net_change: i128 = 0;
        for tx_info in transactions_guard.values() {
            if tx_info.status == TransactionStatus::Pending {
                let tx = &tx_info.transaction;

                for input in &tx.inputs {
                    if let Some(spent_utxo) = utxos_guard.get(&input.outpoint) {
                        if spent_utxo.pubkey == wallet_public_key {
                            pending_net_change -= spent_utxo.value.as_smallest_unit() as i128;
                        }
                    }
                }

                for output in &tx.outputs {
                    if output.pubkey == wallet_public_key {
                        pending_net_change += output.value.as_smallest_unit() as i128;
                    }
                }
            }
        }

        let total_balance_units =
            (confirmed_balance.as_smallest_unit() as i128 + pending_net_change).max(0) as u64;

        Ok(Amount::from_smallest_unit(total_balance_units))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn send_funds(
        &self,
        is_send_max: bool,
        recipient_public_key_str: String,
        amount_to_send: Amount,
        fee_type: FeeType,
        fee_value_raw: u64,
        password: &str,
        _config_path: &PathBuf,
    ) -> Result<()> {
        let current_wallet = self.get_current_wallet().await?;
        let sender_private_key = self
            .decrypt_current_wallet_private_key(password)
            .await
            .context("Incorrect wallet password or decryption failed")?;

        let recipient_public_key = recipient_public_key_str
            .parse::<PublicKey>()
            .context("Invalid recipient public key format")?;

        let intended_fee = if !is_send_max {
            match fee_type {
                FeeType::Fixed => Amount::from_smallest_unit(fee_value_raw),
                FeeType::Percent => Amount::from_smallest_unit(
                    amount_to_send.as_smallest_unit() * fee_value_raw / 10_000,
                ),
            }
        } else {
            Amount::zero()
        };

        let total_required = amount_to_send
            .checked_add(intended_fee)
            .context("Total required amount overflow")?;

        let spendable_utxos = self.utxos.read().await.clone();
        info!(
            "Creating transaction with {} locally known UTXOs.",
            spendable_utxos.len()
        );

        let mut selected_inputs: Vec<TransactionInput> = Vec::new();
        let mut current_input_sum = Amount::zero();

        let mut all_spendable_utxos: Vec<_> = spendable_utxos.iter().collect();
        all_spendable_utxos.sort_by_key(|(_, output)| output.value.as_smallest_unit());

        for (outpoint, utxo_output) in all_spendable_utxos {
            if is_send_max || (current_input_sum < total_required) {
                selected_inputs.push(TransactionInput {
                    outpoint: *outpoint,
                    signature: None,
                    coinbase_data: None,
                });
                current_input_sum = current_input_sum
                    .checked_add(utxo_output.value)
                    .context("Input sum overflow")?;
            } else {
                break;
            }
        }

        if !is_send_max && current_input_sum < total_required {
            return Err(anyhow!(
                "Insufficient funds. Available: {}, Required: {}",
                current_input_sum,
                total_required
            ));
        }

        let (final_amount_to_send, transaction_fee) = if is_send_max {
            let fee = match fee_type {
                FeeType::Fixed => Amount::from_smallest_unit(fee_value_raw), // Fee is fixed
                FeeType::Percent => Amount::from_smallest_unit(
                    current_input_sum.as_smallest_unit() * fee_value_raw / 10_000,
                ),
            };
            let amount = current_input_sum
                .checked_sub(fee)
                .context("Fee calculation underflow for send max")?;
            (amount, fee)
        } else {
            (amount_to_send, intended_fee)
        };

        // Create the transaction outputs.
        let mut outputs: Vec<TransactionOutput> = Vec::new();
        outputs.push(TransactionOutput {
            value: final_amount_to_send,
            pubkey: recipient_public_key,
        });

        let total_to_distribute = final_amount_to_send
            .checked_add(transaction_fee)
            .context("Total amount + fee calculation overflow")?;
        let change_amount = current_input_sum
            .checked_sub(total_to_distribute)
            .context("Change calculation underflow (input sum < amount + fee)")?;

        if change_amount > Amount::zero() {
            outputs.push(TransactionOutput {
                // Send the change back to our own wallet.
                value: change_amount,
                pubkey: current_wallet.public_key,
            });
        }

        // Create the transaction with unsigned inputs.
        let mut new_transaction = Transaction {
            inputs: selected_inputs,
            outputs,
        };

        // Calculate the transaction hash *before* adding signatures. This is the canonical txid.
        let tx_hash_for_signing = new_transaction.txid()?;

        for input in &mut new_transaction.inputs {
            // Sign each input with the private key.
            let signature = wisp_core::signatures::Signature(
                sender_private_key
                    .0
                    .sign(&tx_hash_for_signing.as_bytes()[..]),
            );
            input.signature = Some(signature.clone());
        }

        // Recalculate the txid with the signatures included to get the final, canonical hash.
        let final_txid = new_transaction.txid()?;
        let mut stream_guard = self.get_connected_stream().await?;
        let stream_ref = stream_guard
            .as_mut()
            .expect("Expected an active TCP stream after connection attempt");
        let response_timeout = self.get_node_response_timeout().await;

        // Submit the signed transaction to the node.
        let submit_tx_msg =
            Message::Wallet(WalletMessage::SubmitTransaction(new_transaction.clone()));
        if let Err(e) = submit_tx_msg.send_async(stream_ref).await {
            *stream_guard = None;
            return Err(anyhow!("Failed to send SubmitTransaction message: {}", e));
        }

        let confirmation_response = match tokio::time::timeout(
            response_timeout,
            Message::receive_async(stream_ref),
        )
        .await
        {
            Ok(Ok(msg)) => msg,
            Ok(Err(e)) => {
                *stream_guard = None;
                return Err(anyhow!(
                    "Failed to receive SubmitTransaction confirmation: {}",
                    e
                ));
            }
            Err(_) => {
                *stream_guard = None;
                return Err(anyhow!(
                    "Timeout waiting for SubmitTransaction confirmation"
                ));
            }
        };

        match confirmation_response {
            Message::Wallet(WalletMessage::TransactionAcceptedConfirmation) => {
                info!("Transaction submitted and accepted by node.");
                println!(
                    "Transaction submitted, waiting for confirmation. Hash: {}",
                    final_txid
                );

                // Add the transaction to our local transaction list with a pending status.
                let mut transactions_guard = self.transactions.write().await;
                let tx_info = WalletTransactionInfo {
                    transaction: new_transaction.clone(),
                    status: TransactionStatus::Pending,
                    block_timestamp: Some(Utc::now()),
                    block_index: None,
                };
                transactions_guard.insert(final_txid, tx_info);
                drop(transactions_guard);

                let mut utxos_guard = self.utxos.write().await;
                for input in &new_transaction.inputs {
                    utxos_guard.remove(&input.outpoint);
                }
                for (vout, output) in new_transaction.outputs.iter().enumerate() {
                    if output.pubkey == current_wallet.public_key {
                        // This is our change output, add it to our spendable UTXOs.
                        utxos_guard.insert(
                            OutPoint {
                                txid: final_txid,
                                vout: vout as u32,
                            },
                            output.clone(),
                        );
                    }
                }
            }
            Message::Wallet(WalletMessage::TransactionRejected(hash, reason)) => {
                warn!("Transaction rejected by node: {} - {}", hash, reason);
                return Err(anyhow!("Transaction rejected by node: {}", reason));
            }
            other => {
                *stream_guard = None;
                warn!(
                    "Unexpected response after submitting transaction: {:?}",
                    other
                );
                return Err(anyhow!(
                    "Unexpected response after submitting transaction: {:?}",
                    other
                ));
            }
        }
        Ok(())
    }

    pub async fn get_block_info(&self, index: u64) -> Result<Option<Block>> {
        let mut stream_guard = self.get_connected_stream().await?;
        let stream_ref = stream_guard
            .as_mut()
            .expect("Expected an active TCP stream after connection attempt");
        let response_timeout = self.get_node_response_timeout().await;

        let msg = Message::Chain(ChainMessage::FetchBlockInfo(index));
        if let Err(e) = msg.send_async(stream_ref).await {
            *stream_guard = None;
            return Err(anyhow!("Failed to send FetchBlockInfo message: {}", e));
        }

        let response = match tokio::time::timeout(
            response_timeout,
            Message::receive_async(stream_ref),
        )
        .await
        {
            Ok(Ok(msg)) => msg,
            Ok(Err(e)) => {
                *stream_guard = None;
                return Err(anyhow!("Failed to receive FetchBlockInfo response: {}", e));
            }
            Err(_) => {
                *stream_guard = None;
                return Err(anyhow!("Timeout waiting for FetchBlockInfo response"));
            }
        };

        match response {
            Message::Chain(ChainMessage::BlockInfo(block)) => Ok(block),
            other => {
                *stream_guard = None;
                Err(anyhow!(
                    "Unexpected response for FetchBlockInfo: {:?}",
                    other
                ))
            }
        }
    }

    pub async fn get_latest_block(&self) -> Result<Option<(Block, u64)>> {
        let mut stream_guard = self.get_connected_stream().await?;
        let stream_ref = stream_guard
            .as_mut()
            .expect("Expected an active TCP stream after connection attempt");
        let response_timeout = self.get_node_response_timeout().await;

        let msg = Message::Chain(ChainMessage::FetchLatestBlock);
        if let Err(e) = msg.send_async(stream_ref).await {
            *stream_guard = None;
            return Err(anyhow!("Failed to send FetchLatestBlock message: {}", e));
        }

        let response = match tokio::time::timeout(
            response_timeout,
            Message::receive_async(stream_ref),
        )
        .await
        {
            Ok(Ok(msg)) => msg,
            Ok(Err(e)) => {
                *stream_guard = None;
                return Err(anyhow!(
                    "Failed to receive FetchLatestBlock response: {}",
                    e
                ));
            }
            Err(_) => {
                *stream_guard = None;
                return Err(anyhow!("Timeout waiting for FetchLatestBlock response"));
            }
        };

        match response {
            Message::Chain(ChainMessage::LatestBlock(block_and_height)) => Ok(block_and_height),
            other => {
                *stream_guard = None;
                Err(anyhow!(
                    "Unexpected response for FetchLatestBlock: {:?}",
                    other
                ))
            }
        }
    }

    /// Spawns a background task to periodically sync the wallet state with the node.
    pub async fn start_background_sync(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));

            loop {
                interval.tick().await;

                let wallet_loaded = {
                    let config_guard = self.config.lock().await;
                    if let Some(name) = &config_guard.current_wallet_name {
                        let wallets_guard = self.wallets.lock().await;
                        wallets_guard.iter().any(|w| w.name == *name)
                    } else {
                        false
                    }
                };

                if wallet_loaded {
                    debug!("Background sync: Fetching wallet state...");
                    if let Err(e) = self.fetch_wallet_state().await {
                        warn!("Background sync failed: {}", e);
                    } else {
                        info!("Background sync completed successfully.");
                    }
                } else {
                    debug!("Background sync: No wallet loaded, skipping fetch.");
                }
            }
        });
    }
}
