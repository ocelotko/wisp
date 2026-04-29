use std::path::PathBuf;
use std::sync::Arc;

use crate::{
    engine::session::Core,
    ui::{
        clear_terminal,
        views::layout::{display_seed_phrase, pause},
    },
    utils::{display_heading, prompt_password},
};
use anyhow::{anyhow, Context, Result};
use inquire::{Select, Text};
use log::{error, info};

pub async fn open_wallet_prompt(core: Arc<Core>, config_path: &PathBuf) -> Result<String> {
    info!("Attempting to open a wallet.");
    let available_wallets = Core::load_wallets().await?;
    info!("Available wallets loaded: {:?}", available_wallets);

    if available_wallets.is_empty() {
        println!("No wallets found. Please create one first.");
        pause();
        return Err(anyhow!("No wallets found."));
    }

    let selected_wallet_name = Select::new("Select a wallet:", available_wallets).prompt()?;
    info!("Selected wallet: {}", selected_wallet_name);

    let password = prompt_password("Enter the wallet password:", false)?;
    info!("Password entered for wallet: {}", selected_wallet_name);

    println!("Loading wallet '{}'...", selected_wallet_name);
    match core
        .load_wallet(&selected_wallet_name, &password, config_path)
        .await
    {
        Ok(_) => Ok(selected_wallet_name),
        Err(e) => {
            let is_decryption_error = e
                .chain()
                .any(|cause| cause.to_string().contains("Decryption failed"));
            if is_decryption_error {
                println!("Incorrect password for wallet '{}'.", selected_wallet_name);
            } else {
                error!("Failed to load wallet '{}': {:?}", selected_wallet_name, e);
                println!("Failed to load wallet '{}': {}", selected_wallet_name, e);
            }
            pause();
            Err(e).context(format!("Failed to load wallet '{}'", selected_wallet_name))
        }
    }
}

pub async fn create_wallet_prompt(core: Arc<Core>, config_path: &PathBuf) -> Result<()> {
    clear_terminal();
    display_heading();

    let wallet_file_name = Text::new("Enter a name for your wallet:").prompt()?;
    info!("Wallet name entered for creation: {}", wallet_file_name);
    let password = prompt_password("Create a password: ", true)?;

    info!("Attempting to create wallet: {}", wallet_file_name);
    match core
        .create_wallet(&wallet_file_name, &password, config_path)
        .await
    {
        Ok(mnemonic_phrase) => {
            info!("Wallet '{}' created successfully.", wallet_file_name);
            println!("Wallet '{}' created successfully!", wallet_file_name);
            println!("\nIMPORTANT: SAVE YOUR SEED PHRASE");
            println!("Write down these words in order and keep them safe.");
            println!(
                "This is the ONLY way to recover your wallet if you lose the file or password."
            );
            println!("----------------------------------------------------------------");
            display_seed_phrase(&mnemonic_phrase);
            println!("----------------------------------------------------------------");
            pause();
            Ok(())
        }
        Err(e) => {
            error!("Failed to create wallet '{}': {:?}", wallet_file_name, e);
            println!("Failed to create wallet '{}': {}", wallet_file_name, e);
            pause();
            Err(e).context(format!("Failed to create wallet '{}'", wallet_file_name))
        }
    }
}

pub async fn recover_wallet_with_key_prompt(core: Arc<Core>, config_path: &PathBuf) -> Result<()> {
    let wallet_name = Text::new("Enter a name for your recovered wallet:").prompt()?;
    let password = prompt_password("Create a password for this wallet:", true)?;
    let private_key_hex = Text::new("Enter your private key (hex):").prompt()?;

    info!(
        "Attempting to recover wallet '{}' with private key.",
        wallet_name
    );
    match core
        .recover_wallet_with_key(&wallet_name, &password, &private_key_hex, config_path)
        .await
    {
        Ok(_) => {
            println!("Wallet '{}' recovered successfully!", wallet_name);
            pause();
            Ok(())
        }
        Err(e) => {
            error!("Failed to recover wallet '{}': {}", wallet_name, e);
            println!("Failed to recover wallet: {}", e);
            pause();
            Err(e)
        }
    }
}

pub async fn recover_wallet_with_seed_prompt(core: Arc<Core>, config_path: &PathBuf) -> Result<()> {
    let wallet_name = Text::new("Enter a name for your recovered wallet:").prompt()?;
    let password = prompt_password("Create a password for this wallet:", true)?;
    let seed_phrase = Text::new("Enter your seed phrase (24 words):").prompt()?;

    info!("Attempting to recover wallet '{}' from seed.", wallet_name);
    match core
        .recover_wallet_with_seed(&wallet_name, &password, &seed_phrase, config_path)
        .await
    {
        Ok(_) => {
            println!("Wallet '{}' recovered successfully from seed!", wallet_name);
            pause();
            Ok(())
        }
        Err(e) => {
            error!(
                "Failed to recover wallet '{}' from seed: {}",
                wallet_name, e
            );
            println!("Failed to recover wallet: {}", e);
            pause();
            Err(e)
        }
    }
}
