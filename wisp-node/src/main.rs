use anyhow::{Context, Result};
use argh::FromArgs;
use dashmap::DashMap;
use log::{error, info, warn};
use std::sync::Arc;
use tokio::{
    net::TcpListener,
    net::TcpStream,
    sync::{Mutex as AsyncMutex, OnceCell, RwLock},
};
use wisp_core::blockchain::Blockchain;

pub mod api;
pub mod connection;
pub mod utils;

#[macro_use]
extern crate lazy_static;

#[derive(FromArgs)]
/// A toy blockchain node
/// Defines the command-line arguments for the node application.
struct Args {
    #[argh(option, default = "9000")]
    /// port number
    port: u16,

    #[argh(option, default = "String::from(\"./wisp_db\")")]
    /// sled database directory location
    db_path: String,

    #[argh(positional)]
    /// addresses of initial nodes
    nodes: Vec<String>,
}

lazy_static! {
    /// A global, thread-safe map of connected peer nodes.
    /// The key is the peer's address string, and the value is the TCP stream.
    pub static ref NODES: DashMap<String, Arc<AsyncMutex<TcpStream>>> = DashMap::new();

    /// A global, thread-safe handle to the blockchain state.
    /// `OnceCell` ensures it's initialized only once.
    pub static ref BLOCKCHAIN: OnceCell<Arc<RwLock<Blockchain>>> = OnceCell::new();
}

/// The main function that orchestrates the node's lifecycle.
async fn run_node(port: u16, db_path: String, nodes: Vec<String>) -> Result<()> {
    // Open or create the database for persistent storage.
    let db =
        sled::open(&db_path).with_context(|| format!("Failed to open database at {}", db_path))?;

    let mut blockchain_instance = Blockchain::new(db);

    // Load the blockchain state from the database *before* any network activity.
    // This ensures we know our own state before talking to peers.
    blockchain_instance.load_from_db()?;

    // Set the global BLOCKCHAIN static *before* calling functions that might access it.
    BLOCKCHAIN
        .set(Arc::new(RwLock::new(blockchain_instance)))
        .expect("BUG: BLOCKCHAIN static was already initialized.");

    // Start listening for incoming P2P connections *before* we connect to others.
    // This ensures that if a peer tries to connect back to us during our handshake,
    // we are ready to accept their connection.
    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    info!("Listening on {}", addr);

    // Spawn a separate task for initial peer connection and synchronization.
    // This allows the main task to immediately start accepting connections.
    tokio::spawn(initial_sync_and_discovery(nodes, port));

    let final_height = BLOCKCHAIN.get().unwrap().read().await.block_height()?; // This is now just the local height
    info!(
        "Successfully loaded blockchain with height: {}",
        final_height
    );

    {
        let blockchain_read = crate::BLOCKCHAIN.get().unwrap().read().await;
        if let Some(genesis_hash) = blockchain_read
            .get_block_by_index(0)?
            .and_then(|b| b.id().ok())
        {
            info!("Node Genesis Block Hash: {}", genesis_hash);
        }
    }

    // Clone the blockchain handle for the API server.
    let blockchain_for_api = crate::BLOCKCHAIN.get().unwrap().clone();
    tokio::spawn(async move {
        api::run_api_server(blockchain_for_api).await;
    });

    tokio::spawn(utils::cleanup());

    // The main server loop for accepting new peer connections.
    loop {
        match listener.accept().await {
            Ok((socket, addr)) => {
                info!("Accepted new connection from {}", addr);
                tokio::spawn(async move {
                    let stream_arc = Arc::new(AsyncMutex::new(socket));
                    if let Err(e) = connection::handle_connection(stream_arc, addr).await {
                        error!("Error in connection handler from {}: {:?}", addr, e);
                    }
                });
            }
            Err(e) => {
                error!("Error accepting connection: {}", e);
            }
        }
    }
}

/// A separate async function to handle the initial connection and sync logic.
/// This is spawned as a background task so it doesn't block the main connection listener.
async fn initial_sync_and_discovery(nodes: Vec<String>, self_port: u16) {
    // A small delay to ensure the listener is fully up.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    if let Err(e) = utils::populate_connections(&nodes, self_port).await {
        warn!("Error during initial peer population: {}", e);
    }

    info!("Total amount of known nodes: {}", crate::NODES.len());

    // If initial peer nodes were provided, check if we need to sync from them.
    if !nodes.is_empty() {
        info!("Checking for longer chain against initial nodes...");
        let (longest_name, longest_count) = match {
            let bc = BLOCKCHAIN.get().unwrap().read().await;
            utils::find_longest_chain_node(&bc).await
        } {
            Ok(result) => result,
            Err(e) => {
                error!("Failed to find longest chain node: {}", e);
                return;
            }
        };

        let local_chain_length = match BLOCKCHAIN.get().unwrap().read().await.block_height() {
            Ok(h) => h + 1,
            Err(e) => {
                error!("Failed to get local chain height: {}", e);
                return;
            }
        };

        if longest_count > local_chain_length {
            info!(
                "Peer {} has a longer chain ({} blocks), preparing to download...",
                longest_name, longest_count
            );

            if let Some(mut peer) = crate::NODES.get_mut(&longest_name) {
                let mut stream = peer.value_mut().lock().await;
                if let Err(e) = utils::download_blockchain_with_existing_stream(
                    &mut stream,
                    &longest_name,
                    longest_count,
                )
                .await
                {
                    error!("Initial blockchain download failed: {:?}", e);
                }
            } else {
                warn!(
                    "Could not find peer {} in connection map to start download.",
                    longest_name
                );
            }
        }
    }
}
/// The application entry point.
#[tokio::main]
async fn main() -> Result<()> {
    // Use try_init to avoid panics if the logger is already initialized,
    // which can happen in a workspace with multiple binaries.
    let _ = env_logger::builder()
        .filter_module("wisp_node", log::LevelFilter::Info)
        .try_init();

    let args: Args = argh::from_env();

    run_node(args.port, args.db_path, args.nodes).await
}
