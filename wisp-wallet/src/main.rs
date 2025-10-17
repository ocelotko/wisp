#[path = "commands/mod.rs"]
pub mod commands;
mod utils;
mod wallet;

use anyhow::Result;
use clap::Parser;
use log::info;
use std::{path::PathBuf, sync::Arc};

use crate::{commands::run_wallet_ui, wallet::core::Core};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    info!("Starting Wisp Wallet...");

    let cli = Cli::parse();
    let config_path = cli.config;

    let core = Core::load(config_path.clone()).await?;

    run_wallet_ui(Arc::new(core), config_path).await?;

    info!("Wisp Wallet exited cleanly.");
    Ok(())
}
