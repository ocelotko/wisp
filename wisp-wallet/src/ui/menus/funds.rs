use crate::engine::session::Core;
use crate::engine::tx::FeeType;
use crate::ui::clear_terminal;
use crate::ui::views::layout::{display_heading_with_wallet, pause};
use crate::utils::prompt_password;
use anyhow::{anyhow, Context, Result};
use inquire::{Confirm, Select, Text};
use log::{error, info};
use std::path::PathBuf;
use std::sync::Arc;

// New enum for address display options
enum AddressDisplayOption {
    FirstAurora,
    AllAurora,
    AllFormats,
}

pub async fn funds_management_loop(core: Arc<Core>, config_path: &PathBuf) -> Result<()> {
    loop {
        core.fetch_wallet_state().await?;
        clear_terminal();
        let summary = core.get_wallet_summary().await.ok();
        let wallet_name = core.get_current_wallet().await.ok().map(|w| w.name);
        display_heading_with_wallet(wallet_name.as_deref(), summary.map(|s| s.total_balance));

        let send_receive_options = vec![
            "Send funds",
            "Receive funds",
            "Transaction history",
            "Back to wallet menu",
        ];
        let send_receive_menu_selection =
            Select::new("Send and Receive Funds", send_receive_options).prompt()?;

        match send_receive_menu_selection.as_ref() {
            "Send funds" => {
                send_funds_prompt(Arc::clone(&core), config_path).await?;
            }
            "Receive funds" => receive_funds_view(&core, AddressDisplayOption::FirstAurora).await?,
            "Transaction history" => {
                if let Err(e) =
                    crate::ui::views::history::transaction_history_view(Arc::clone(&core)).await
                {
                    error!("Transaction history failed: {}", e);
                    println!("\nError: {}", e);
                    pause();
                }
            }
            "Back to wallet menu" => return Ok(()),
            _ => {
                error!("Send/Receive menu selection error: Invalid selection or prompt error.");
                println!("Invalid selection or prompt error.");
                pause();
            }
        }
    }
}

pub async fn address_management_loop(core: Arc<Core>) -> Result<()> {
    loop {
        clear_terminal();
        let summary = core.get_wallet_summary().await.ok();
        let wallet_name = core.get_current_wallet().await.ok().map(|w| w.name);
        display_heading_with_wallet(wallet_name.as_deref(), summary.map(|s| s.total_balance));

        let options = vec![
            "Show all addresses",
            "Show legacy addresses",
            "Generate new address",
            "Back to wallet menu",
        ];
        let selection = Select::new("Address Management", options).prompt()?;

        match selection {
            "Show all addresses" => {
                receive_funds_view(&core, AddressDisplayOption::AllAurora).await?
            }
            "Show legacy addresses" => {
                receive_funds_view(&core, AddressDisplayOption::AllFormats).await?
            }
            "Generate new address" => {
                let password =
                    prompt_password("Enter wallet password to derive new address:", false)?;
                let (index, addresses) = core.generate_new_address(&password).await?;
                println!("\n--- Successfully generated Address #{} ---", index + 1);
                if let Some((label, addr)) = addresses.iter().find(|(l, _)| l.contains("Aurora")) {
                    println!("{} {}", label, addr);
                }

                println!("\nOther formats (Shadow/Classic) have also been derived");
                println!("and are now being watched by your wallet.");
                pause();
            }
            "Back to wallet menu" => return Ok(()),
            _ => {}
        }
    }
}

async fn send_funds_prompt(core: Arc<Core>, _config_path: &PathBuf) -> Result<()> {
    clear_terminal();
    let summary = core.get_wallet_summary().await.ok();
    let wallet_name = core.get_current_wallet().await.ok().map(|w| w.name);
    display_heading_with_wallet(wallet_name.as_deref(), summary.map(|s| s.total_balance));

    let recipient_address_str =
        Text::new("Enter recipient's address (Aurora, Shadow, or Classic):")
            .prompt()
            .context("Failed to read recipient's address")?;

    if !Confirm::new("Is this address correct?")
        .with_default(true)
        .prompt()?
    {
        println!("Operation cancelled.");
        pause();
        return Ok(());
    }

    let send_options = vec![
        "Enter specific amount",
        "Sweep all funds (Send Max)",
        "Send Dust (Min allowed)",
    ];
    let send_option_selection = Select::new("Amount to send:", send_options).prompt()?;

    let (is_send_max, amount_str) = match send_option_selection {
        "Enter specific amount" => {
            let s = Text::new("Enter amount to send (e.g., 1 or 0.5 Wisp):")
                .prompt()
                .context("Failed to read amount")?;
            (false, s)
        }
        "Sweep all funds (Send Max)" => (true, "0".to_string()),
        "Send Dust (Min allowed)" => (false, wisp_core::MIN_OUTPUT_VALUE.to_string()),
        _ => return Err(anyhow!("Invalid selection")),
    };

    let fee_type: FeeType =
        Select::new("Select fee type:", vec![FeeType::Fixed, FeeType::Percent]).prompt()?;

    let fee_value_str = Text::new(
        format!(
            "Enter fee value (for {}: e.g., 0.01 for Fixed Wisp; 0.0-100.0 for Percentage):",
            fee_type
        )
        .as_str(),
    )
    .prompt()
    .context("Failed to read fee value")?;

    let password = prompt_password("Enter your wallet password:", false)?;

    info!(
        "Initiating fund transfer logic for recipient: {}",
        recipient_address_str
    );
    println!("Processing transaction...");

    match core
        .send_funds(
            is_send_max,
            recipient_address_str,
            &amount_str,
            fee_type,
            &fee_value_str,
            &password,
            _config_path,
        )
        .await
    {
        Ok(_) => println!("Funds sent successfully!"),
        Err(e) => {
            println!("\nError sending funds: {}", e);
        }
    };

    pause();
    Ok(())
}

async fn receive_funds_view(core: &Core, option: AddressDisplayOption) -> Result<()> {
    clear_terminal();
    let summary = core.get_wallet_summary().await.ok();
    let wallet_name = core.get_current_wallet().await.ok().map(|w| w.name);
    display_heading_with_wallet(wallet_name.as_deref(), summary.map(|s| s.total_balance));

    match core.get_current_wallet().await {
        Ok(wallet) => {
            let addresses = core.get_receive_addresses(&wallet)?;

            println!("\n--- Your Wallet Addresses ---");
            match option {
                AddressDisplayOption::FirstAurora => {
                    if let Some((label, address)) =
                        addresses.iter().find(|(l, _)| l.contains("Aurora"))
                    {
                        println!("{} {}", label, address);
                    } else {
                        println!("No Aurora address found.");
                    }
                }
                AddressDisplayOption::AllAurora => {
                    for (label, address) in addresses.iter().filter(|(l, _)| l.contains("Aurora")) {
                        println!("{} {}", label, address);
                    }
                }
                AddressDisplayOption::AllFormats => {
                    let count = addresses.len();
                    for (i, (label, address)) in addresses.into_iter().enumerate() {
                        println!("{} {}", label, address);
                        // Group addresses by key (Aurora, Shadow, Classic) with a separator
                        if (i + 1) % 3 == 0 && (i + 1) < count {
                            println!("{}", "-".repeat(75));
                        }
                    }
                }
            }
        }
        Err(_) => println!("No wallet loaded."),
    }
    pause();
    Ok(())
}
