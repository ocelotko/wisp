use crate::utils::{clear_terminal, display_heading_with_wallet, pause, prompt_password};
use crate::wallet::core::{Core, FeeType};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use inquire::{Confirm, Select, Text};
use log::error;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use wisp_core::address::Address;
use wisp_core::currency::Amount;
use wisp_core::network::TransactionStatus;
use wisp_core::sha256::Hash;
use wisp_core::transactions::Script;

pub async fn funds_management(core: Arc<Core>, config_path: &PathBuf) -> Result<()> {
    loop {
        core.fetch_wallet_state().await?;
        clear_terminal();
        let balance_value = core.get_total_balance().await;
        display_heading_with_wallet(
            core.get_current_wallet()
                .await
                .ok()
                .map(|w| w.name)
                .as_deref(),
            balance_value.as_ref().ok().copied(),
        );

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
            "Receive funds" => receive_funds(&core).await?,
            "Transaction history" => {
                if let Err(e) = transaction_history(Arc::clone(&core)).await {
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

pub async fn address_management(core: Arc<Core>) -> Result<()> {
    loop {
        clear_terminal();
        let balance_value = core.get_total_balance().await;
        display_heading_with_wallet(
            core.get_current_wallet()
                .await
                .ok()
                .map(|w| w.name)
                .as_deref(),
            balance_value.ok(),
        );

        let options = vec![
            "Show my addresses (All formats)",
            "View current watch-list",
            "Add address to watch-list",
            "Remove address from watch-list",
            "Back to wallet menu",
        ];
        let selection = Select::new("Address Management", options).prompt()?;

        match selection {
            "Show my addresses (All formats)" => receive_funds(&core).await?,
            "View current watch-list" => {
                let wallet = core.get_current_wallet().await?;
                if wallet.script_hashes.is_empty() {
                    println!("\nYour watch-list is empty.");
                } else {
                    println!("\n--- Current Watch-List ---");
                    for h in &wallet.script_hashes {
                        println!("- {}", Address::encode(&Script::AuroraScript(*h)));
                    }
                }
                pause();
            }
            "Add address to watch-list" => {
                let addr = Text::new("Enter Wisp address to watch:").prompt()?;
                let password = prompt_password("Enter wallet password to save changes:", false)?;
                match core.add_watched_address(&addr, &password).await {
                    Ok(_) => {
                        println!("Address successfully added to watch-list.");
                        pause();
                    }
                    Err(e) => {
                        println!("Error: {}", e);
                        pause();
                    }
                }
            }
            "Remove address from watch-list" => {
                let addr = Text::new("Enter Wisp address to remove:").prompt()?;
                let password = prompt_password("Enter wallet password to save changes:", false)?;
                match core.remove_watched_address(&addr, &password).await {
                    Ok(_) => {
                        println!("Address successfully removed from watch-list.");
                        pause();
                    }
                    Err(e) => {
                        println!("Error: {}", e);
                        pause();
                    }
                }
            }
            "Back to wallet menu" => return Ok(()),
            _ => {
                return Ok(());
            }
        }
    }
}

async fn send_funds_prompt(core: Arc<Core>, _config_path: &PathBuf) -> Result<()> {
    clear_terminal();
    let balance_value = core.get_total_balance().await;
    display_heading_with_wallet(
        core.get_current_wallet()
            .await
            .ok()
            .map(|w| w.name)
            .as_deref(),
        balance_value.ok(),
    );

    let recipient_public_key_str = Text::new("Enter recipient's public key (hex):")
        .prompt()
        .context("Failed to read recipient's public key")?;

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

    let (is_send_max, amount_obj) = match send_option_selection {
        "Enter specific amount" => {
            let amount_str = Text::new("Enter amount to send (e.g., 1 or 0.5 Wisp):")
                .prompt()
                .context("Failed to read amount")?;
            let amount = Amount::from_string_wisp(&amount_str)
                .context("Invalid amount entered. Please use a number, e.g., 1.23")?;
            (false, amount)
        }
        "Sweep all funds (Send Max)" => (true, Amount::zero()),
        "Send Dust (Min allowed)" => (
            false,
            Amount::from_smallest_unit(wisp_core::MIN_OUTPUT_VALUE),
        ),
        _ => return Err(anyhow!("Invalid selection")),
    };

    let fee_type: FeeType =
        Select::new("Select fee type:", vec![FeeType::Fixed, FeeType::Percent]).prompt()?;

    let fee_value_str_input = Text::new(
        format!(
            "Enter fee value (for {}: e.g., 0.01 for Fixed Wisp; 0.0-100.0 for Percentage):",
            fee_type
        )
        .as_str(),
    )
    .prompt()
    .context("Failed to read fee value")?;

    let mut fee_value_for_core: u64;

    match fee_type {
        FeeType::Fixed => {
            let fixed_fee_amount = Amount::from_string_wisp(&fee_value_str_input)
                .context("Invalid fixed fee value entered. Please use a number, e.g., 0.01 Wisp")?;
            fee_value_for_core = fixed_fee_amount.as_smallest_unit();
        }
        FeeType::Percent => {
            let parts: Vec<&str> = fee_value_str_input.split('.').collect();
            let integer_part_str = parts[0];
            let fractional_part_str = if parts.len() > 1 { parts[1] } else { "" };

            if fractional_part_str.len() > 2 {
                return Err(anyhow!("Percentage fee supports up to two decimal places."));
            }

            let integer_part = if integer_part_str.is_empty() {
                0
            } else {
                integer_part_str
                    .parse::<u64>()
                    .context("Invalid integer part of percentage")?
            };

            let mut fractional_part = if fractional_part_str.is_empty() {
                0
            } else {
                fractional_part_str
                    .parse::<u64>()
                    .context("Invalid fractional part of percentage")?
            };

            if fractional_part_str.len() == 1 {
                fractional_part *= 10;
            }

            fee_value_for_core = integer_part
                .saturating_mul(100)
                .saturating_add(fractional_part);

            if fee_value_for_core > 10_000 {
                fee_value_for_core = 10_000;
            }
        }
    }

    match core
        .send_funds(
            is_send_max,
            recipient_public_key_str,
            amount_obj,
            fee_type,
            fee_value_for_core,
            &prompt_password("Enter your wallet password:", false)?,
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

async fn receive_funds(core: &Core) -> Result<(), anyhow::Error> {
    clear_terminal();
    let balance_value = core.get_total_balance().await;
    display_heading_with_wallet(
        core.get_current_wallet()
            .await
            .ok()
            .map(|w| w.name)
            .as_deref(),
        balance_value.ok(),
    );

    match core.get_current_wallet().await {
        Ok(wallet) => {
            let pk = wallet.public_key;
            let hash_160 = wisp_core::address::Address::hash160(&pk);
            let mut h_bytes = [0u8; 32];
            h_bytes[..20].copy_from_slice(&hash_160);
            let h = Hash::from_bytes(&h_bytes);

            println!("\n--- Your Wallet Addresses ---");
            println!(
                "Aurora (Modern/SegWit):  {}",
                Address::encode(&Script::Aurora(h))
            );
            println!(
                "Shadow (Standard):       {}",
                Address::encode(&Script::Shadow(h))
            );
            println!(
                "Classic (Legacy/Keys):   {}",
                Address::encode(&Script::Classic(pk))
            );

            println!("\nNote: Aurora is recommended for lower fees and better privacy.");
            if !wallet.script_hashes.is_empty() {
                println!(
                    "(Watching {} additional custom scripts)",
                    wallet.script_hashes.len()
                );
            }
        }
        Err(_) => println!("No wallet loaded."),
    }
    pause();
    Ok(())
}

struct TxDisplayItem {
    timestamp: DateTime<Utc>,
    display_date_time: String,
    tx_type: String,
    amount_str: String,
    counterparty_info: String,
    tx_hash: wisp_core::sha256::Hash,
    is_pending: bool,
}

const TRANSACTIONS_PER_PAGE: usize = 5;

async fn transaction_history(core: Arc<Core>) -> Result<(), anyhow::Error> {
    clear_terminal();
    let balance_value = core.get_total_balance().await;
    display_heading_with_wallet(
        core.get_current_wallet()
            .await
            .ok()
            .map(|w| w.name)
            .as_deref(),
        balance_value.as_ref().ok().copied(),
    );

    let current_wallet = core.get_current_wallet().await?;
    let wallet_public_key = current_wallet.public_key.clone();
    let pk_hash_bytes = wisp_core::address::Address::hash160(&wallet_public_key);

    println!(
        "Fetching transaction history for wallet: {}",
        wallet_public_key.fingerprint()
    );

    let mut display_items: Vec<TxDisplayItem> = Vec::new();

    let all_txs = core.transactions.read().await;
    let script_hashes = &current_wallet.script_hashes;

    for tx_info in all_txs.values() {
        let tx = &tx_info.transaction;
        let tx_hash = tx.txid()?;
        let is_pending = tx_info.status == TransactionStatus::Pending;
        let is_coinbase = tx.is_coinbase();

        let display_date_time = tx_info
            .block_timestamp
            .unwrap_or_else(Utc::now)
            .format("%d.%m.%Y %H:%M:%S")
            .to_string();

        let mut value_from_us = Amount::zero();
        let mut value_to_us = Amount::zero();

        for input in &tx.inputs {
            if let Some(source_tx) = all_txs.get(&input.outpoint.txid) {
                if let Some(spent_output) = source_tx
                    .transaction
                    .outputs
                    .get(input.outpoint.vout as usize)
                {
                    let is_mine = spent_output.script.is_relevant_to(
                        &wallet_public_key,
                        &pk_hash_bytes,
                        script_hashes,
                    );
                    if is_mine {
                        value_from_us = value_from_us
                            .checked_add(spent_output.value)
                            .context("Overflow calculating value from us in transaction history")?;
                    }
                }
            }
        }

        for output in &tx.outputs {
            let is_mine =
                output
                    .script
                    .is_relevant_to(&wallet_public_key, &pk_hash_bytes, script_hashes);
            if is_mine {
                value_to_us = value_to_us
                    .checked_add(output.value)
                    .context("value_to_us overflow")?;
            }
        }

        if value_from_us == Amount::zero() && value_to_us == Amount::zero() {
            continue;
        }

        let net_effect =
            (value_to_us.as_smallest_unit() as i128) - (value_from_us.as_smallest_unit() as i128);

        let (tx_type, amount_str, counterparty_info) = if is_coinbase {
            (
                "Coinbase".to_string(),
                format!("+{} WISP", value_to_us.to_string_wisp()),
                "Coinbase Reward".to_string(),
            )
        } else if net_effect < 0 {
            let recipients: HashSet<_> = tx
                .outputs
                .iter()
                .filter(|o| {
                    !o.script
                        .is_relevant_to(&wallet_public_key, &pk_hash_bytes, script_hashes)
                })
                .map(|o| wisp_core::address::Address::encode(&o.script))
                .collect();

            let amount_sent_to_others: Amount = tx
                .outputs
                .iter()
                .filter(|o| {
                    !o.script
                        .is_relevant_to(&wallet_public_key, &pk_hash_bytes, script_hashes)
                })
                .map(|o| o.value)
                .sum();

            let total_output_value = amount_sent_to_others
                .checked_add(value_to_us)
                .context("Total output value overflowed")?;
            let fee = value_from_us.checked_sub(total_output_value);

            let recipient_info = if recipients.is_empty() {
                "Self (fee only)".to_string()
            } else if recipients.len() == 1 {
                recipients.iter().next().unwrap().to_string()
            } else {
                format!("{} recipients", recipients.len())
            };
            (
                "Sent".to_string(),
                if let Some(fee_val) = fee {
                    format!(
                        "-{} (Fee: {})",
                        amount_sent_to_others.to_string_wisp(),
                        fee_val.to_string_wisp()
                    )
                } else {
                    format!("-{} WISP", amount_sent_to_others.to_string_wisp())
                },
                recipient_info,
            )
        } else {
            let amount_received = Amount::from_smallest_unit(net_effect.abs() as u64);
            let senders: HashSet<_> = tx
                .inputs
                .iter()
                .filter_map(|i| all_txs.get(&i.outpoint.txid).map(|info| (i, info)))
                .flat_map(|(i, source_tx_info)| {
                    source_tx_info
                        .transaction
                        .outputs
                        .get(i.outpoint.vout as usize)
                        .into_iter()
                        .filter(|o| {
                            !o.script.is_relevant_to(
                                &wallet_public_key,
                                &pk_hash_bytes,
                                script_hashes,
                            )
                        })
                        .map(|o| wisp_core::address::Address::encode(&o.script))
                })
                .collect();

            let sender_info = if senders.is_empty() {
                "Unknown".to_string()
            } else if senders.len() == 1 {
                senders.iter().next().unwrap().to_string()
            } else {
                format!("{} senders", senders.len())
            };
            (
                "Received".to_string(),
                format!("+{} WISP", amount_received.to_string_wisp()),
                sender_info,
            )
        };

        display_items.push(TxDisplayItem {
            timestamp: tx_info.block_timestamp.unwrap_or_else(Utc::now),
            display_date_time,
            tx_type,
            amount_str,
            counterparty_info,
            tx_hash,
            is_pending,
        });
    }

    display_items.sort_by(|a, b| {
        if a.is_pending && !b.is_pending {
            std::cmp::Ordering::Less
        } else if !a.is_pending && b.is_pending {
            std::cmp::Ordering::Greater
        } else {
            b.timestamp.cmp(&a.timestamp)
        }
    });

    // --- Pagination and Display Loop ---
    let total_pages = (display_items.len() + TRANSACTIONS_PER_PAGE - 1) / TRANSACTIONS_PER_PAGE;
    let mut current_page = 0;

    loop {
        clear_terminal();
        display_heading_with_wallet(
            core.get_current_wallet()
                .await
                .ok()
                .map(|w| w.name)
                .as_deref(),
            balance_value.as_ref().ok().copied(),
        );

        println!(
            "--- Transaction History (Page {}/{}) ---",
            current_page + 1,
            total_pages
        );
        println!(
            "{:<20} {:<10} {:<10} {:>25}  {:<30}",
            "Date/Time", "Status", "Type", "Amount", "Counterparty/Memo"
        );
        println!("{}", "-".repeat(105));

        let start_index = current_page * TRANSACTIONS_PER_PAGE;
        let end_index = (start_index + TRANSACTIONS_PER_PAGE).min(display_items.len());

        if display_items.is_empty() {
            println!("\nNo transactions found for this wallet.");
        } else if start_index >= display_items.len() && !display_items.is_empty() {
            println!("\nNo more transactions on this page.");
        }

        for tx_item in display_items[start_index..end_index].iter() {
            let status_str = if tx_item.is_pending {
                "Pending"
            } else {
                "Confirmed"
            };
            let color_code = if tx_item.is_pending {
                "\x1B[33m"
            } else if tx_item.amount_str.starts_with('+') {
                "\x1B[32m"
            } else {
                "\x1B[31m"
            };
            let reset_color = "\x1B[0m";

            println!(
                "{}{:<20} {:<10} {:<10} {:>25}  {:<30}{}",
                color_code,
                tx_item.display_date_time,
                status_str,
                tx_item.tx_type,
                tx_item.amount_str,
                tx_item.counterparty_info,
                reset_color
            );
            println!(
                "  \x1B[90mhttp://localhost:8000/transaction/{}\x1B[0m",
                tx_item.tx_hash.to_string()
            );
            println!();
        }
        println!("{}", "-".repeat(105));

        let mut page_options = Vec::new();
        if current_page > 0 {
            page_options.push("Previous Page");
        }
        if current_page < total_pages.saturating_sub(1) {
            page_options.push("Next Page");
        }
        page_options.push("Back to Wallet Menu");

        if page_options.is_empty() {
            pause();
            return Ok(());
        }

        let selection = Select::new("Navigation", page_options).prompt()?;

        match selection {
            "Next Page" => current_page += 1,
            "Previous Page" => current_page = current_page.saturating_sub(1),
            "Back to Wallet Menu" => return Ok(()),
            _ => {}
        }
    }
}
