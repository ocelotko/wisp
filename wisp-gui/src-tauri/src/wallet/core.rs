use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use std::{fs, sync::Arc};

use anyhow::{anyhow, Context, Result};
use log::{debug, info, warn};
use tokio::sync::{Mutex as AsyncMutex, MutexGuard};
use tokio::time::timeout;
use tokio::{net::TcpStream, sync::RwLock};

use wisp_core::{
    network::WalletTransactionInfo,
    sha256::Hash,
    transactions::{OutPoint, TransactionOutput},
};

use crate::wallet::{config::Config, storage::SavedWallet};

pub struct Core {
    pub config: Arc<AsyncMutex<Config>>,
    pub wallets: Arc<AsyncMutex<Vec<SavedWallet>>>,
    pub discovered_nodes: Arc<AsyncMutex<HashMap<String, Option<Duration>>>>,
    pub utxos: Arc<RwLock<HashMap<OutPoint, TransactionOutput>>>,
    pub transactions: Arc<RwLock<HashMap<Hash, WalletTransactionInfo>>>,
    pub data_dir: PathBuf,
    pub(crate) connected_node_stream: Arc<AsyncMutex<Option<TcpStream>>>,
}

impl Core {
    pub async fn load(config_path: PathBuf) -> Result<Self> {
        let data_dir = config_path
            .parent()
            .unwrap_or(&PathBuf::from("."))
            .to_path_buf();

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
            data_dir,
            connected_node_stream: Arc::new(AsyncMutex::new(None)),
        })
    }

    pub async fn save_config(&self, path: &PathBuf, config_data: &Config) -> Result<()> {
        let config_string =
            toml::to_string_pretty(config_data).context("Failed to serialize config to TOML")?;

        tokio::fs::write(path, config_string)
            .await
            .context("Failed to write config file")?;

        info!("Config saved to {:?}", path);
        Ok(())
    }

    pub async fn get_default_node_address(&self) -> String {
        let config_guard = self.config.lock().await;
        config_guard.default_node.clone()
    }

    pub async fn get_node_response_timeout(&self) -> Duration {
        let config_guard = self.config.lock().await;
        Duration::from_secs(config_guard.node_response_timeout_secs)
    }

    pub(crate) async fn get_connected_stream(&self) -> Result<MutexGuard<'_, Option<TcpStream>>> {
        let node_address = self.get_default_node_address().await;
        let connect_timeout =
            Duration::from_secs(self.config.lock().await.node_connect_timeout_secs);

        let mut stream_lock = self.connected_node_stream.lock().await;

        if stream_lock.is_none() {
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
}
