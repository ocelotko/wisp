use anyhow::Result;
use log::debug;
use tokio::net::TcpStream;
use wisp_core::network::{Message, P2PMessage};

/// Handles a `DiscoverNodes` request from a peer.
///
/// It responds with a `NodeList` message containing the addresses of all
/// currently connected peers known to this node.
pub async fn handle_discover_nodes(stream: &mut TcpStream) -> Result<()> {
    debug!("Handling DiscoverNodes request.");

    let nodes: Vec<String> = crate::NODES
        .iter()
        .map(|peer_ref| peer_ref.key().clone())
        .collect();
    Message::P2P(P2PMessage::NodeList(nodes))
        .send_async(stream)
        .await?;

    Ok(())
}

/// Handles a `Ping` message from a peer.
///
/// It immediately responds with a `Pong` message. This serves as a basic
/// keep-alive and latency check mechanism.
pub async fn handle_ping(stream: &mut TcpStream) -> Result<()> {
    debug!("Received Ping, sending Pong.");
    Message::P2P(P2PMessage::Pong).send_async(stream).await?;
    Ok(())
}
