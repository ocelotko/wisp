// wisp-wallet/src/wallet/config.rs
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub default_node: String,
    pub current_wallet_name: Option<String>,
    pub seed_nodes: Vec<String>,

    #[serde(default = "default_node_discovery_interval_secs")]
    pub node_discovery_interval_secs: u64,

    #[serde(default = "default_max_nodes_to_discover_per_run")]
    pub max_nodes_to_discover_per_run: usize,

    #[serde(default = "default_node_connect_timeout_secs")]
    pub node_connect_timeout_secs: u64,

    #[serde(default = "default_node_response_timeout_secs")]
    pub node_response_timeout_secs: u64,
}

fn default_node_discovery_interval_secs() -> u64 {
    60
}
fn default_max_nodes_to_discover_per_run() -> usize {
    10
}
fn default_node_connect_timeout_secs() -> u64 {
    5
}
fn default_node_response_timeout_secs() -> u64 {
    10
}

impl Default for Config {
    fn default() -> Self {
        Config {
            default_node: "0.0.0.0:9000".to_string(),
            current_wallet_name: None,
            seed_nodes: vec!["0.0.0.0:9000".to_string()],
            node_discovery_interval_secs: default_node_discovery_interval_secs(),
            max_nodes_to_discover_per_run: default_max_nodes_to_discover_per_run(),
            node_connect_timeout_secs: default_node_connect_timeout_secs(),
            node_response_timeout_secs: default_node_response_timeout_secs(),
        }
    }
}
