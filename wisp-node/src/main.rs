use anyhow::{Context, Result};
use argh::FromArgs;
use dashmap::DashMap;
use log::{error, info};
use std::sync::Arc;
use tokio::{net::TcpListener, net::TcpStream, sync::OnceCell, sync::RwLock};
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
    pub static ref NODES: DashMap<String, TcpStream> = DashMap::new();

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

    // Load the blockchain state from the database, or initialize a new one with a genesis block.
    blockchain_instance.load_from_db()?;

    if blockchain_instance.block_height()? > 0 {
        info!(
            "Successfully loaded blockchain with height: {}",
            blockchain_instance.block_height()?
        );
    } else {
        info!("Initialized new blockchain with genesis block.");
    }

    // Set the global BLOCKCHAIN static so other parts of the application can access it.
    crate::BLOCKCHAIN
        .set(std::sync::Arc::new(tokio::sync::RwLock::new(
            blockchain_instance,
        )))
        .expect("BUG: BLOCKCHAIN static was already initialized.");

    utils::populate_connections(&nodes).await?;
    info!("Total amount of known nodes: {}", crate::NODES.len());

    // If initial peer nodes are provided, synchronize the blockchain.
    if !nodes.is_empty() {
        info!("Checking for longer chain against initial nodes...");
        // Find which of our known peers has the longest chain.
        let (longest_name, longest_count) = utils::find_longest_chain_node().await?;

        let local_chain_length = crate::BLOCKCHAIN
            .get()
            .unwrap()
            .read()
            .await
            .block_height()?
            + 1;

        // If a peer has a longer chain, download the missing blocks.
        if longest_count > local_chain_length {
            info!(
                "Peer {} has a longer chain ({} blocks), preparing to download...",
                longest_name, longest_count
            );

            if crate::NODES.remove(&longest_name).is_some() {
                info!(
                    "Closed idle connection to {} before starting download.",
                    longest_name
                );
            }

            match utils::download_blockchain(&longest_name, longest_count).await {
                Ok(_) => {
                    info!("Blockchain download completed successfully.");
                }
                Err(e) => {
                    error!("Blockchain download failed: {:?}", e);
                }
            }
        } else {
            info!("Local blockchain is up-to-date or longer.");
        }
    } else {
        info!("No initial nodes provided. Starting with local blockchain state.");
    }
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

    // Start listening for incoming P2P connections.
    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    info!("Listening on {}", addr);

    tokio::spawn(utils::cleanup());

    // The main server loop for accepting new peer connections.
    loop {
        match listener.accept().await {
            Ok((socket, addr)) => {
                info!("Accepted new connection from {}", addr);
                tokio::spawn(async move {
                    if let Err(e) = connection::handle_connection(socket, addr).await {
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
