use anyhow::{anyhow, Context, Result};
use log::{debug, info, warn};
use std::{sync::Arc, time::Duration};
use wisp_core::network::{Message, WalletMessage};

use crate::engine::session::Core;

impl Core {
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

    pub async fn fetch_wallet_state(&self) -> Result<()> {
        let current_wallet = self.get_current_wallet().await?;
        let wallet_public_key = current_wallet.public_key.clone();
        info!(
            "Fetching wallet state for public key: {}",
            wallet_public_key.fingerprint()
        );

        let known_script_hashes = current_wallet.script_hashes.clone();

        let response_timeout = self.get_node_response_timeout().await;

        let mut stream_guard = self
            .get_connected_stream()
            .await
            .context("Failed to get connected stream for wallet state fetch")?;

        debug!("fetch_wallet_state: Connected stream obtained.");
        let stream_ref = stream_guard
            .as_mut()
            .expect("Expected an active TCP stream after connection attempt");

        let fetch_state_msg = Message::Wallet(WalletMessage::FetchWalletState(
            wallet_public_key.clone(),
            known_script_hashes,
        ));

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
}
