mod menus;
mod views;

use inquire::Select;
use log::{error, info};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;

use crate::engine::session::Core;
use crate::ui::menus::main;
use crate::ui::menus::wallet::wallet_management_loop;
use crate::ui::views::layout::pause;
use crate::utils::{display_heading, exit_program};

pub fn clear_terminal() {
    print!("\x1B[2J\x1B[1;1H");
    io::stdout().flush().unwrap();
}

pub fn check_terminal_size() -> bool {
    if let Some((width, height)) = termion::terminal_size().ok() {
        if width < 80 || height < 20 {
            println!("Terminal too small. Please resize to at least 80x20.");
            return false;
        }
    }
    true
}

pub async fn run_wallet_ui(core: Arc<Core>, config_path: PathBuf) -> Result<(), anyhow::Error> {
    loop {
        clear_terminal();
        display_heading();

        if !check_terminal_size() {
            pause();
            continue;
        }

        let main_options = vec![
            "Open wallet",
            "Create new wallet",
            "Recover wallet from seed",
            "Recover wallet with private key",
            "Exit",
        ];

        let main_menu_selection = Select::new("Main Menu", main_options).prompt();

        match main_menu_selection.as_deref() {
            Ok("Open wallet") => {
                info!("Attempting to open wallet from main menu.");
                if let Err(e) = main::open_wallet_prompt(Arc::clone(&core), &config_path).await {
                    error!("Failed to open wallet: {}", e);
                    println!("\nFailed to open wallet: {}", e);
                    pause();
                } else {
                    info!("Wallet loaded successfully. Proceeding to wallet management.");
                    if let Err(e) = wallet_management_loop(Arc::clone(&core), &config_path).await {
                        error!("Wallet management exited with error: {}", e);
                        println!("\nWallet management error: {}", e);
                        pause();
                    }
                }
            }

            Ok("Create new wallet") => {
                info!("Attempting to create new wallet from main menu.");
                if let Err(e) = main::create_wallet_prompt(Arc::clone(&core), &config_path).await {
                    error!("Failed to create wallet: {}", e);
                    println!("\nFailed to create wallet: {}", e);
                    pause();
                } else {
                    info!("Wallet created successfully. Proceeding to wallet management.");
                    if let Err(e) = wallet_management_loop(Arc::clone(&core), &config_path).await {
                        error!("Wallet management exited with error: {}", e);
                        println!("\nWallet management error: {}", e);
                        pause();
                    }
                }
            }

            Ok("Recover wallet with private key") => {
                info!("Attempting to recover wallet with private key from main menu.");
                if let Err(e) =
                    main::recover_wallet_with_key_prompt(Arc::clone(&core), &config_path).await
                {
                    error!("Failed to recover wallet: {}", e);
                    println!("\nFailed to recover wallet: {}", e);
                    pause();
                } else {
                    info!("Wallet recovered successfully. Proceeding to wallet management.");
                    if let Err(e) = wallet_management_loop(Arc::clone(&core), &config_path).await {
                        error!("Wallet management exited with error: {}", e);
                    }
                }
            }

            Ok("Recover wallet from seed") => {
                info!("Attempting to recover wallet from seed from main menu.");
                if let Err(e) =
                    main::recover_wallet_with_seed_prompt(Arc::clone(&core), &config_path).await
                {
                    error!("Failed to recover wallet from seed: {}", e);
                    println!("\nFailed to recover wallet: {}", e);
                    pause();
                } else {
                    info!("Wallet recovered successfully. Proceeding to wallet management.");
                    if let Err(e) = wallet_management_loop(Arc::clone(&core), &config_path).await {
                        error!("Wallet management exited with error: {}", e);
                    }
                }
            }
            Ok("Exit") => exit_program(),
            Err(e) => {
                error!("Main menu selection error: {}", e);
                println!("Invalid selection: {}", e);
                pause();
            }
            _ => { /* Should not happen with inquire::Select */ }
        }
    }
}
