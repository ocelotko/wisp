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
/// Wisp blockchain node
struct Args {
    #[argh(option, default = "9000")]
    /// port number for P2P communication.
    port: u16,

    #[argh(option, default = "String::from(\"0.0.0.0\")")]
    /// address to bind the P2P listener to.
    bind_addr: String,

    #[argh(switch)]
    /// disable UPnP port forwarding.
    no_upnp: bool,

    #[argh(option, default = "3001")]
    /// port number for the JSON-RPC API server.
    api_port: u16,

    #[argh(option, default = "String::from(\"0.0.0.0\")")]
    /// address to bind the API server to.
    api_bind_addr: String,

    #[argh(switch)]
    /// disable the JSON-RPC API server.
    disable_api: bool,

    #[argh(option, default = "String::from(\"./wisp_db\")")]
    /// sled database directory location.
    db_path: String,

    #[argh(switch)]
    /// perform database migration to fix total supply.
    migrate_db: bool,

    #[argh(positional)]
    /// addresses of initial nodes to connect to.
    nodes: Vec<String>,

    #[argh(option, default = "30")]
    /// interval in seconds for cleaning up old transactions from the mempool (0 to disable).
    mempool_cleanup_interval: u64,
}

lazy_static! {
    pub static ref NODES: DashMap<String, Arc<AsyncMutex<TcpStream>>> = DashMap::new();
    pub static ref BLOCKCHAIN: OnceCell<Arc<RwLock<Blockchain>>> = OnceCell::new();
    pub static ref PUBLIC_ADDR: OnceCell<String> = OnceCell::new();
}

async fn run_node(args: Args) -> Result<()> {
    let db = sled::open(&args.db_path)
        .with_context(|| format!("Failed to open database at {}", args.db_path))?;

    let mut blockchain_instance = Blockchain::new(db);

    if args.migrate_db {
        info!("Starting database migration...");
        blockchain_instance.migrate_total_supply()?;
    }

    blockchain_instance.load_from_db()?;

    // Pre-warm DAA cache to speed up target calculation and API responses.
    let height = blockchain_instance.block_height()?;
    let start = height.saturating_sub(wisp_core::DAA_WINDOW as u64);
    info!(
        "Pre-warming DAA cache from block {} to {}...",
        start, height
    );
    for i in start..=height {
        if let Some(block) = blockchain_instance.get_block_by_index(i)? {
            blockchain_instance
                .daa_cache
                .insert(i, (block.header.timestamp, block.header.target));
        }
    }

    // Attempt UPnP port forwarding to get a public IP address.
    if !args.no_upnp {
        info!("Attempting UPnP port forwarding...");
        let port_for_upnp = args.port;
        let port_mapping_result = tokio::task::spawn_blocking(move || {
            match igd_next::search_gateway(Default::default()) {
                Ok(gateway) => {
                    let ip = gateway.get_external_ip()?;

                    // Discover local IP to forward to.
                    let local_ip = std::net::UdpSocket::bind("0.0.0.0:0")
                        .and_then(|s| {
                            s.connect("8.8.8.8:80")?;
                            s.local_addr()
                        })
                        .map(|addr| addr.ip())
                        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));

                    gateway.add_port(
                        igd_next::PortMappingProtocol::TCP,
                        port_for_upnp,
                        std::net::SocketAddr::new(local_ip, port_for_upnp),
                        0,
                        "wisp-node",
                    )?;
                    Ok::<_, anyhow::Error>(ip)
                }
                Err(e) => Err(anyhow::anyhow!(e)),
            }
        })
        .await?;

        if let Ok(ip) = port_mapping_result {
            info!("UPnP successful. External IP: {}", ip);
            let _ = PUBLIC_ADDR.set(format!("{}:{}", ip, args.port));
        } else {
            warn!("UPnP failed. Node might not be reachable from the internet without manual port forwarding.");
        }
    } else {
        info!("UPnP is disabled by configuration.");
    }

    BLOCKCHAIN
        .set(Arc::new(RwLock::new(blockchain_instance)))
        .expect("BUG: BLOCKCHAIN static was already initialized.");

    let addr = format!("{}:{}", args.bind_addr, args.port);
    let listener = TcpListener::bind(&addr).await?;
    info!("Listening on {}", addr);

    tokio::spawn(initial_sync_and_discovery(args.nodes, args.port));

    let final_height = BLOCKCHAIN.get().unwrap().read().await.block_height()?;
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

    if !args.disable_api {
        let blockchain_for_api = crate::BLOCKCHAIN.get().unwrap().clone();
        let api_addr_str = format!("{}:{}", args.api_bind_addr, args.api_port);
        let api_addr: std::net::SocketAddr = api_addr_str
            .parse()
            .with_context(|| format!("Invalid API address format: {}", api_addr_str))?;

        tokio::spawn(async move {
            api::run_api_server(blockchain_for_api, api_addr).await;
        });
    } else {
        info!("API server is disabled by configuration.");
    }

    tokio::spawn(utils::cleanup(args.mempool_cleanup_interval));

    tokio::select! {
        _ = async {
            loop {
                match listener.accept().await {
                    Ok((socket, addr)) => {
                        info!("Accepted new connection from {}", addr);
                        tokio::spawn(async move {
                            let stream_arc = Arc::new(AsyncMutex::new(socket));
                            if let Err(e) = connection::handle_connection(stream_arc, addr, None).await {
                                error!("Error in connection handler from {}: {:?}", addr, e);
                            }
                        });
                    }
                    Err(e) => {
                        error!("Error accepting connection: {}", e);
                    }
                }
            }
        } => {},
        _ = tokio::signal::ctrl_c() => {
            info!("Shutdown signal received. Stopping node...");
        }
    }

    if let Some(bc) = BLOCKCHAIN.get() {
        info!("Saving mempool snapshot...");
        let blockchain = bc.read().await;
        if let Err(e) = blockchain.save_mempool_snapshot() {
            error!("Failed to save mempool snapshot: {}", e);
        }
    }

    Ok(())
}

/// A separate async function to handle the initial connection and sync logic.
/// This is spawned as a background task so it doesn't block the main connection listener.
async fn initial_sync_and_discovery(nodes: Vec<String>, self_port: u16) {
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    if !nodes.is_empty() {
        let blockchain = BLOCKCHAIN.get().unwrap().read().await;
        let local_chain_height = blockchain.block_height().unwrap_or(0);

        let longest_peer_info =
            match utils::populate_connections(&nodes, self_port, &blockchain).await {
                Ok(info) => info,
                Err(e) => {
                    warn!("Error during initial peer population: {}", e);
                    None
                }
            };

        info!(
            "Finished initial peer discovery. Total known nodes: {}",
            crate::NODES.len()
        );

        if let Some((longest_name, longest_height)) = longest_peer_info {
            let longest_count = longest_height + 1;
            let local_chain_length = local_chain_height + 1;

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
}
#[tokio::main]
async fn main() -> Result<()> {
    let _ = env_logger::builder()
        .filter_module("wisp_node", log::LevelFilter::Info)
        .try_init();

    let args: Args = argh::from_env();

    run_node(args).await
}
