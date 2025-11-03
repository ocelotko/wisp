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
use chrono::Utc;
use k256::ecdsa::signature::Signer;
use k256::ecdsa::{self, SigningKey};
use log::{debug, info, warn};
use rand::rngs::OsRng;
use rand::TryRngCore;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;
use tokio::{net::TcpStream, sync::RwLock};

use wisp_core::{
    blockchain::Block,
    currency::Amount,
    network::WalletTransactionInfo,
    network::{Message, TransactionStatus},
    sha256::Hash,
    signatures::{PrivateKey, PublicKey},
    transactions::{OutPoint, Transaction, TransactionInput, TransactionOutput},
};

use crate::wallet::{config::Config, constants::*, storage::SavedWallet};

/// Defines the method for calculating transaction fees.
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

/// The central struct for managing wallet state and operations.
/// It holds configuration, wallet data, and network connection state.
pub struct Core {
    /// Wallet configuration settings.
    pub config: Arc<AsyncMutex<Config>>,
    /// A list of loaded wallets.
    pub wallets: Arc<AsyncMutex<Vec<SavedWallet>>>,
    /// A map of discovered network peers and their last seen time.
    pub discovered_nodes: Arc<AsyncMutex<HashMap<String, Option<Duration>>>>,
    /// The wallet's current set of spendable Unspent Transaction Outputs (UTXOs).
    pub utxos: Arc<RwLock<HashMap<OutPoint, TransactionOutput>>>,
    pub transactions: Arc<RwLock<HashMap<Hash, WalletTransactionInfo>>>,
    /// The active TCP stream to the connected node.
    connected_node_stream: Arc<AsyncMutex<Option<TcpStream>>>,
}

impl Core {
    /// Loads the wallet core, reading configuration from a file or using defaults.
    pub async fn load(config_path: PathBuf) -> Result<Self> {
        // Try to load config from file, otherwise create a default config.
        let config = match fs::read_to_string(&config_path) {
            Ok(content) => toml::from_str(&content)?,
            Err(_) => {
                info!("No config file found, using default configuration.");
                Config::default()
            }
        };

        // Initialize the Core struct with empty state.
        Ok(Core {
            config: Arc::new(AsyncMutex::new(config)),
            wallets: Arc::new(AsyncMutex::new(Vec::new())),
            discovered_nodes: Arc::new(AsyncMutex::new(HashMap::new())),
            utxos: Arc::new(RwLock::new(HashMap::new())),
            transactions: Arc::new(RwLock::new(HashMap::new())),
            connected_node_stream: Arc::new(AsyncMutex::new(None)), // Initialize as None
        })
    }

    /// Helper to get the currently connected node address from config.
    async fn get_default_node_address(&self) -> String {
        let config_guard = self.config.lock().await;
        config_guard.default_node.clone()
    }

    /// Helper to get the response timeout from config.
    pub async fn get_node_response_timeout(&self) -> Duration {
        let config_guard = self.config.lock().await;
        Duration::from_secs(config_guard.node_response_timeout_secs)
    }

    /// Gets a handle to the connected node stream.
    /// If not connected, it establishes a new connection to the default node.
    /// This function ensures that there is only one active connection at a time.
    pub async fn get_connected_stream(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, Option<TcpStream>>> {
        let mut stream_lock = self.connected_node_stream.lock().await;

        if let Some(ref stream) = *stream_lock {
            if stream.peer_addr().is_ok() {
                debug!("Re-using existing connection to node.");
                return Ok(stream_lock);
            } else {
                warn!("Existing connection is dead, re-connecting.");
                *stream_lock = None;
            }
        }

        let node_address = self.get_default_node_address().await;
        let connect_timeout =
            Duration::from_secs(self.config.lock().await.node_connect_timeout_secs);

        info!("Attempting to connect to node at: {}", node_address);
        let stream = timeout(connect_timeout, TcpStream::connect(&node_address))
            .await
            .map_err(|e| anyhow!("Connection timed out to {}: {}", node_address, e))?
            .map_err(|e| anyhow!("Failed to connect to node {}: {}", node_address, e))?;

        info!("Successfully connected to node at {}", node_address);
        *stream_lock = Some(stream);
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

    /// Creates a new wallet, encrypts its private key, and saves it to a file.
    pub async fn create_wallet(
        &self,
        name: &str,
        password: &str,
        config_path: &PathBuf,
    ) -> Result<()> {
        let mut wallets_guard = self.wallets.lock().await;
        if wallets_guard.iter().any(|w| w.name == name) {
            return Err(anyhow!("Wallet with name '{}' already exists", name));
        }

        // Generate a new private/public key pair.
        let private_key =
            PrivateKey::generate_keypair_with_rng(&mut ecdsa::signature::rand_core::OsRng);
        let public_key = private_key.public_key();

        println!("Public Key: {:?}", public_key);

        // Use a local OsRng for salt generation
        let mut local_rng = OsRng;
        let mut salt = vec![0u8; SALT_SIZE];
        local_rng
            .try_fill_bytes(&mut salt)
            .map_err(|e| anyhow!("Failed to fill bytes for salt: {}", e))?;

        // Encrypt the private key using a key derived from the password and salt.
        let encrypted_private_key = Self::encrypt_private_key(&private_key, password, &salt)?;

        let new_wallet = SavedWallet {
            name: name.to_string(),
            encrypted_private_key,
            public_key,
            salt,
        };

        // Save the new wallet to its own file.
        new_wallet.save_to_file(password)?;

        // Set the newly created wallet as the current one in the config.
        {
            let mut config_guard = self.config.lock().await;
            config_guard.current_wallet_name = Some(name.to_string());
            self.save_config(config_path, &*config_guard).await?;
        }

        // Load the newly created wallet into memory
        let loaded_wallet = SavedWallet::load_from_file(name, password)?;
        wallets_guard.push(loaded_wallet);

        info!("Wallet '{}' created successfully!", name);
        Ok(())
    }

    /// Recovers a wallet from a raw private key, creating a new encrypted wallet file.
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

    /// Scans the wallet directory and returns a list of available wallet names.
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

    /// Loads a specific wallet into memory by decrypting its file with the provided password.
    pub async fn load_wallet(
        &self,
        name: &str,
        password: &str,
        config_path: &PathBuf,
    ) -> Result<()> {
        info!("Loading wallet: {}", name);

        let name_clone = name.to_string();
        let password_clone = password.to_string();

        // Decryption is CPU-intensive, so it's done in a blocking thread.
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

        // Check if wallet is already loaded to avoid duplicates
        if wallets_guard.iter().any(|w| w.name == name) {
            info!("Wallet '{}' is already loaded.", name);
            drop(wallets_guard);

            let mut config_guard = self.config.lock().await; // Acquire lock here
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

    /// Returns the currently active wallet.
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

    /// A convenience method to decrypt the private key of the currently loaded wallet.
    pub async fn decrypt_current_wallet_private_key(&self, password: &str) -> Result<PrivateKey> {
        let current_wallet = self.get_current_wallet().await?;
        Self::decrypt_private_key(
            &current_wallet.encrypted_private_key,
            password,
            &current_wallet.salt,
        )
    }

    /// Encrypts a private key using AES-256-GCM with a key derived from a password and salt.
    pub fn encrypt_private_key(
        private_key: &PrivateKey,
        password: &str,
        salt: &[u8],
    ) -> Result<String> {
        let mut rng = OsRng;
        let mut nonce = [0u8; ENCRYPTION_NONCE_SIZE]; // Use a local OsRng for nonce generation
        rng.try_fill_bytes(&mut nonce)
            .map_err(|e| anyhow!("Failed to fill bytes for nonce: {}", e))?;
        let nonce = Nonce::from(nonce);

        // Derive a 256-bit key from the password and salt using Argon2.
        let key = SavedWallet::derive_key(password, salt)?;
        let cipher = Aes256Gcm::new_from_slice(&key).expect("key is correct length");

        let private_key_bytes = private_key.0.to_bytes();
        let ciphertext = cipher
            .encrypt(&nonce, &private_key_bytes[..])
            .map_err(|e| anyhow!("Encryption failed: {}", e))?;

        // Prepend the nonce to the ciphertext and Base64-encode the result.
        let combined = [&nonce[..], &ciphertext[..]].concat();

        Ok(general_purpose::STANDARD_NO_PAD.encode(&combined))
    }

    /// Decrypts a private key. This is the reverse of `encrypt_private_key`.
    fn decrypt_private_key(
        encrypted_private_key: &str,
        password: &str,
        salt: &[u8],
    ) -> Result<PrivateKey> {
        let decoded = general_purpose::STANDARD_NO_PAD.decode(encrypted_private_key)?;
        if decoded.len() <= ENCRYPTION_NONCE_SIZE {
            return Err(anyhow!("Invalid encrypted private key format"));
        }
        let nonce_array: [u8; ENCRYPTION_NONCE_SIZE] = decoded[..ENCRYPTION_NONCE_SIZE]
            .try_into()
            .expect("Nonce slice has incorrect length, this should be prevented by earlier check");
        let nonce = Nonce::from(nonce_array);
        let ciphertext = &decoded[ENCRYPTION_NONCE_SIZE..];

        let key = SavedWallet::derive_key(password, salt)?;
        let cipher = Aes256Gcm::new_from_slice(&key).expect("key is correct length");

        // Decrypt the data. AES-GCM's authenticated encryption means this will fail if the key
        // (derived from the password) is incorrect, because the authentication tag won't match.
        let decrypted_bytes = match cipher.decrypt(&nonce, ciphertext) {
            Ok(bytes) => bytes,
            Err(_) => return Err(anyhow!("Incorrect password or corrupted wallet file.")),
        };

        // As a final check, ensure the decrypted bytes can be parsed into a valid private key.
        // it means the password was wrong.
        SigningKey::from_slice(&decrypted_bytes)
            .map(PrivateKey)
            .map_err(|_| anyhow!("Incorrect password or corrupted wallet file."))
    }

    /// Changes the password for the current wallet.
    pub async fn change_wallet_password(
        &self,
        current_password: &str,
        new_password: &str,
    ) -> Result<()> {
        // 1. Get the current wallet's data. This acquires and releases the lock.
        let wallet_data = self.get_current_wallet().await?;

        // 2. Decrypt the private key with the current password to validate it.
        let private_key = Core::decrypt_private_key(
            &wallet_data.encrypted_private_key,
            current_password,
            &wallet_data.salt,
        )
        .context("Incorrect current password or decryption failed")?;

        // 3. Acquire the lock to modify the in-memory wallet data.
        let mut wallets_guard = self.wallets.lock().await;

        if let Some(wallet_to_update) = wallets_guard
            .iter_mut()
            .find(|w| w.name == wallet_data.name)
        {
            // 4. Generate a new salt and re-encrypt the key with the new password.
            let mut rng = OsRng;
            let mut new_salt = vec![0u8; SALT_SIZE];
            rng.try_fill_bytes(&mut new_salt)
                .map_err(|e| anyhow!("Failed to fill bytes for new salt: {}", e))?;

            wallet_to_update.salt = new_salt.clone();
            wallet_to_update.encrypted_private_key =
                Self::encrypt_private_key(&private_key, new_password, &new_salt)?;

            // 5. Save the updated wallet to its file in a blocking task.
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
            println!("🔑 Wallet password changed successfully.");
        } else {
            // This case is unlikely if get_current_wallet succeeded, but it's good practice.
            return Err(anyhow!(
                // This case is unlikely if get_current_wallet succeeded, but it's good practice.
                "Current wallet disappeared from memory during password change."
            ));
        }
        Ok(())
    }

    pub async fn delete_wallet(&self, name: &str, config_path: &PathBuf) -> Result<()> {
        // Delete the wallet file from disk.
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
            // Remove the wallet from the in-memory list.
            let mut wallets_guard = self.wallets.lock().await;
            wallets_guard.retain(|w| w.name != name);
            Ok(())
        } else {
            Err(anyhow!("Failed to delete wallet file '{}'", name))
        }
    }

    /// Fetches an atomic snapshot of the wallet's state from the node.
    /// This includes all UTXOs and the status of pending transactions.
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

        // Send the request to the node.
        let fetch_state_msg = Message::FetchWalletState(wallet_public_key.clone());
        fetch_state_msg
            .send_async(stream_ref)
            .await
            .context("Failed to send FetchWalletState message")?;
        debug!("fetch_wallet_state: Waiting for WalletState response.");

        let state_response =
            tokio::time::timeout(response_timeout, Message::receive_async(stream_ref))
                .await
                .context("Timeout waiting for WalletState response")??;

        match state_response {
            Message::WalletState(snapshot) => {
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
                return Err(anyhow!(
                    "Unexpected response for FetchWalletState: {:?}",
                    other
                ));
            }
        }

        info!("fetch_wallet_state: Wallet state fetch completed successfully.");
        Ok(())
    }

    /// Calculates and returns the total spendable balance from the available UTXOs.
    pub async fn get_total_balance(&self) -> Result<Amount> {
        let available_utxos_guard = self.utxos.read().await;
        available_utxos_guard
            .values()
            .try_fold(Amount::zero(), |acc, output| acc + output.value) // This returns a Result
            .context("An arithmetic error occurred during balance calculation")
    }

    /// Creates, signs, and submits a transaction to send funds.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_funds(
        &self,
        is_send_max: bool,
        recipient_public_key_str: String,
        amount_to_send: Amount, // This is already in smallest units
        fee_type: FeeType,
        fee_value_raw: u64, // For fixed: in smallest units; for percent: in basis points (e.g., 500 for 5.00%)
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

        // For a regular send, calculate the fee and total required amount upfront.
        // For "send max", we don't know the total yet, so we'll calculate it after selecting all inputs.
        let total_required = if !is_send_max {
            let fee = match fee_type {
                FeeType::Fixed => Amount::from_smallest_unit(fee_value_raw),
                FeeType::Percent => Amount::from_smallest_unit(
                    amount_to_send.as_smallest_unit() * fee_value_raw / 10_000,
                ),
            };
            (amount_to_send + fee).context("Total required amount overflow")?
        } else {
            // For send_max, we need all UTXOs, so the initial requirement is effectively infinite until we sum them up.
            Amount::MAX
        };

        // Fetch a fresh snapshot of spendable UTXOs from the node.
        // We use the locally cached UTXOs. The node will validate them upon submission.
        // A pre-fetch here is redundant as the node is the final arbiter.
        let spendable_utxos = self.utxos.read().await.clone();
        info!(
            "Creating transaction with {} locally known UTXOs.",
            spendable_utxos.len()
        );

        let mut selected_inputs: Vec<TransactionInput> = Vec::new();
        let mut current_input_sum = Amount::zero();

        // Select the smallest UTXOs first until the total required amount is met (coin selection).
        let mut all_spendable_utxos: Vec<_> = spendable_utxos.iter().collect();
        all_spendable_utxos.sort_by_key(|(_, output)| output.value.as_smallest_unit());

        for (outpoint, utxo_output) in all_spendable_utxos {
            // For "send max", we take all UTXOs. For regular sends, we stop when we have enough.
            if is_send_max || current_input_sum < total_required {
                selected_inputs.push(TransactionInput {
                    outpoint: *outpoint,
                    signature: None,
                    coinbase_data: None,
                });
                current_input_sum = (current_input_sum + utxo_output.value)?;
            } else {
                break; // Stop once we have enough value
            }
        }

        // Now that we have our inputs, verify we have enough funds for a regular send.
        if !is_send_max && current_input_sum < total_required {
            return Err(anyhow!(
                "Insufficient funds. Available: {}, Required: {}",
                current_input_sum,
                total_required
            ));
        }
        let (final_amount_to_send, transaction_fee) = if is_send_max {
            // For "send max", the fee is calculated from the total available input value.
            let fee = match fee_type {
                FeeType::Fixed => Amount::from_smallest_unit(fee_value_raw),
                FeeType::Percent => {
                    // For "send max", the percentage fee should be calculated from the total available input value.
                    let fee_amount = current_input_sum.as_smallest_unit() * fee_value_raw / 10_000;
                    Amount::from_smallest_unit(fee_amount)
                }
            };
            let amount = (current_input_sum - fee)?;
            (amount, fee)
        } else {
            // For regular sends, the fee is calculated based on the amount the user wants to send.
            let fee = match fee_type {
                FeeType::Fixed => Amount::from_smallest_unit(fee_value_raw),
                FeeType::Percent => Amount::from_smallest_unit(
                    amount_to_send.as_smallest_unit() * fee_value_raw / 10_000,
                ),
            };
            (amount_to_send, fee)
        };

        // Create the transaction outputs.
        let mut outputs: Vec<TransactionOutput> = Vec::new();
        outputs.push(TransactionOutput {
            value: final_amount_to_send,
            pubkey: recipient_public_key,
        });

        // Calculate if change is needed and create a change output if necessary.
        let total_spent = (final_amount_to_send + transaction_fee)?;
        let change_amount = (current_input_sum - total_spent)?;
        if change_amount > Amount::zero() {
            outputs.push(TransactionOutput {
                // Send the change back to our own wallet.
                value: change_amount,
                pubkey: current_wallet.public_key,
            });
        }

        // --- Signing and Submission ---
        // Create the transaction with unsigned inputs.
        let mut new_transaction = Transaction {
            inputs: selected_inputs,
            outputs,
        };

        let transaction_hash_for_signing = new_transaction.txid()?;
        for input in &mut new_transaction.inputs {
            // Sign each input with the private key.
            let signature = wisp_core::signatures::Signature(
                sender_private_key
                    .0
                    .sign(&transaction_hash_for_signing.as_bytes()[..]),
            );
            input.signature = Some(signature.clone());
        }

        let mut stream_guard = self.get_connected_stream().await?;
        let stream_ref = stream_guard
            .as_mut()
            .expect("Expected an active TCP stream after connection attempt");
        let response_timeout = self.get_node_response_timeout().await;

        // Submit the signed transaction to the node.
        let submit_tx_msg = Message::SubmitTransaction(new_transaction.clone());
        submit_tx_msg
            .send_async(stream_ref)
            .await
            .context("Failed to send SubmitTransaction message")?;

        let confirmation_response =
            tokio::time::timeout(response_timeout, Message::receive_async(stream_ref))
                .await
                .context("Timeout waiting for SubmitTransaction confirmation")??;

        // Handle the node's response.
        match confirmation_response {
            Message::TransactionAcceptedConfirmation => {
                info!("Transaction submitted and accepted by node.");

                // The transaction was accepted by the node, so now we can safely
                // update our local state to reflect this. This prevents the UI from
                // showing a pending transaction that was actually rejected.

                let tx_hash = new_transaction.txid()?;
                println!(
                    "🚀 Transaction submitted, waiting for confirmation. Hash: {}",
                    tx_hash
                );

                // Add the transaction to our local transaction list with a pending status.
                let tx_info = WalletTransactionInfo {
                    transaction: new_transaction,
                    status: TransactionStatus::Pending,
                    block_timestamp: Some(Utc::now()),
                    block_index: None,
                };
                self.transactions.write().await.insert(tx_hash, tx_info);

                // Re-fetch the wallet state to get the updated UTXO set from the node.
                self.fetch_wallet_state().await?;
            }
            Message::TransactionRejected(hash, reason) => {
                warn!("Transaction rejected by node: {} - {}", hash, reason);
                return Err(anyhow!("Transaction rejected by node: {}", reason));
            }
            other => {
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

    /// Fetches block information from the node.
    pub async fn get_block_info(&self, index: u64) -> Result<Option<Block>> {
        let mut stream_guard = self.get_connected_stream().await?;
        let stream_ref = stream_guard
            .as_mut()
            .expect("Expected an active TCP stream after connection attempt");
        let response_timeout = self.get_node_response_timeout().await;

        let msg = Message::FetchBlockInfo(index);
        msg.send_async(stream_ref)
            .await
            .context("Failed to send FetchBlockInfo message")?;

        let response = tokio::time::timeout(response_timeout, Message::receive_async(stream_ref))
            .await
            .context("Timeout waiting for FetchBlockInfo response")??;

        match response {
            Message::BlockInfo(block) => Ok(block),
            other => Err(anyhow!(
                "Unexpected response for FetchBlockInfo: {:?}",
                other
            )),
        }
    }

    /// Fetches the latest block from the node.
    pub async fn get_latest_block(&self) -> Result<Option<(Block, u64)>> {
        let mut stream_guard = self.get_connected_stream().await?;
        let stream_ref = stream_guard
            .as_mut()
            .expect("Expected an active TCP stream after connection attempt");
        let response_timeout = self.get_node_response_timeout().await;

        let msg = Message::FetchLatestBlock;
        msg.send_async(stream_ref)
            .await
            .context("Failed to send FetchLatestBlock message")?;

        let response = tokio::time::timeout(response_timeout, Message::receive_async(stream_ref))
            .await
            .context("Timeout waiting for FetchLatestBlock response")??;

        match response {
            Message::LatestBlock(block_and_height) => Ok(block_and_height),
            other => Err(anyhow!(
                "Unexpected response for FetchLatestBlock: {:?}",
                other
            )),
        }
    }
}
