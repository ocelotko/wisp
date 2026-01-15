use crate::utils::{clear_terminal, display_heading, display_seed_phrase, pause, prompt_password};
use crate::wallet::core::Core;
use anyhow::{anyhow, Context, Result};
use bip39::Mnemonic;
use inquire::{Select, Text};
use log::{error, info};
use std::path::PathBuf;
use std::sync::Arc;

pub async fn open_wallet(core: Arc<Core>, config_path: &PathBuf) -> Result<String> {
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

pub async fn prompt_create_wallet(core: Arc<Core>, config_path: &PathBuf) -> Result<()> {
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

pub async fn prompt_recover_wallet_with_key(core: Arc<Core>, config_path: &PathBuf) -> Result<()> {
    clear_terminal();
    display_heading();
    println!("--- Recover Wallet with Private Key ---");
    println!("WARNING: This will create a new wallet file from an existing private key.");

    let private_key_hex = Text::new("Enter your private key (hex):")
        .prompt()
        .context("Failed to read private key")?;

    let wallet_file_name = Text::new("Enter a new name for this wallet:")
        .prompt()
        .context("Failed to read wallet name")?;

    let password = prompt_password("Create a new password for this wallet: ", true)?;

    info!(
        "Attempting to recover wallet '{}' from private key.",
        wallet_file_name
    );
    match core
        .recover_wallet_with_key(&wallet_file_name, &password, &private_key_hex, config_path)
        .await
    {
        Ok(_) => {
            println!("Wallet '{}' recovered successfully!", wallet_file_name);
            pause();
            Ok(())
        }
        Err(e) => {
            println!("Failed to recover wallet: {}", e);
            pause();
            Err(e).context("Failed to recover wallet")
        }
    }
}

pub async fn prompt_recover_wallet_with_seed(core: Arc<Core>, config_path: &PathBuf) -> Result<()> {
    clear_terminal();
    display_heading();
    println!("--- Recover Wallet from Seed Phrase ---");
    println!("WARNING: This will create a new wallet file from a mnemonic seed phrase.");

    let input_method = Select::new(
        "Choose input method:",
        vec!["Paste full phrase", "Enter word-by-word"],
    )
    .prompt()?;

    let seed_phrase = match input_method {
        "Paste full phrase" => {
            let input = Text::new("Enter your seed phrase:")
                .with_help_message("Separate words with spaces.")
                .prompt()
                .context("Failed to read seed phrase")?;
            input.trim().to_string()
        }
        "Enter word-by-word" => {
            let lengths = vec!["12 words", "24 words"];
            let length_selection = Select::new("Select phrase length:", lengths).prompt()?;
            let count = if length_selection.starts_with("12") {
                12
            } else {
                24
            };

            let mut words = Vec::with_capacity(count);
            println!("\nEnter words one by one:");
            for i in 1..=count {
                loop {
                    let word = Text::new(&format!("Word #{}:", i)).prompt()?;
                    let trimmed = word.trim();
                    if !trimmed.is_empty() {
                        words.push(trimmed.to_string());
                        break;
                    }
                    println!("Word cannot be empty. Please try again.");
                }
            }
            words.join(" ")
        }
        _ => return Err(anyhow!("Invalid selection")),
    };

    // Validate mnemonic before proceeding
    if let Err(e) = Mnemonic::parse(&seed_phrase) {
        println!("\nError: Invalid seed phrase.");
        println!("Details: {}", e);
        println!("Please check your words and try again.");
        pause();
        return Ok(());
    }

    println!("\nSeed phrase is valid!");

    let wallet_file_name = Text::new("Enter a new name for this wallet:")
        .prompt()
        .context("Failed to read wallet name")?;

    let password = prompt_password("Create a new password for this wallet: ", true)?;

    info!(
        "Attempting to recover wallet '{}' from seed phrase.",
        wallet_file_name
    );
    match core
        .recover_wallet_with_seed(&wallet_file_name, &password, &seed_phrase, config_path)
        .await
    {
        Ok(_) => {
            println!("Wallet '{}' recovered successfully!", wallet_file_name);
            pause();
            Ok(())
        }
        Err(e) => {
            println!("Failed to recover wallet: {}", e);
            pause();
            Err(e).context("Failed to recover wallet")
        }
    }
}
