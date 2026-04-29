use anyhow::{anyhow, Result};
use log::{debug, info, warn};
use std::{path::PathBuf, time::Duration};
use tokio::{net::TcpStream, sync::MutexGuard, time::timeout};
use wisp_core::{
    blockchain::Block,
    network::{ChainMessage, Message},
};

use crate::engine::session::Core;

impl Core {
    pub async fn get_connected_stream(&self) -> Result<MutexGuard<'_, Option<TcpStream>>> {
        let (node_address, connect_timeout) = {
            let config = self.config.lock().await;
            (
                config.default_node.clone(),
                Duration::from_secs(config.node_connect_timeout_secs),
            )
        };

        let mut stream_lock = self.connected_node_stream.lock().await;

        // Check if an existing stream is still healthy
        let is_alive = if let Some(ref stream) = *stream_lock {
            stream.peer_addr().is_ok()
        } else {
            false
        };

        if !is_alive {
            *stream_lock = None;
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

    pub async fn set_default_node(
        &self,
        new_node_address: &str,
        config_path: &PathBuf,
    ) -> Result<()> {
        {
            let mut config_guard = self.config.lock().await;
            config_guard.default_node = new_node_address.to_string();
            // The config_guard is passed by reference, so it's unlocked after the call.
            self.save_config(config_path, &*config_guard).await?;
        }

        // Invalidate current connection so next call to get_connected_stream will use the new address
        let mut stream_lock = self.connected_node_stream.lock().await;
        *stream_lock = None;

        info!("Default node address updated to: {}", new_node_address);
        Ok(())
    }

    pub async fn fetch_peers_from_node(&self) -> Result<Vec<String>> {
        let response_timeout = self.get_node_response_timeout().await;
        let mut stream_guard = self.get_connected_stream().await?;
        let stream_ref = stream_guard
            .as_mut()
            .expect("Expected an active TCP stream after connection attempt");

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

    pub async fn get_block_info(&self, index: u64) -> Result<Option<Block>> {
        let response_timeout = self.get_node_response_timeout().await;
        let mut stream_guard = self.get_connected_stream().await?;
        let stream_ref = stream_guard
            .as_mut()
            .expect("Expected an active TCP stream after connection attempt");

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
        let response_timeout = self.get_node_response_timeout().await;
        let mut stream_guard = self.get_connected_stream().await?;
        let stream_ref = stream_guard
            .as_mut()
            .expect("Expected an active TCP stream after connection attempt");

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
}
