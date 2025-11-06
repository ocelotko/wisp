use crate::utils::{clear_terminal, display_heading_with_wallet, pause, prompt_password};
use crate::wallet::core::{Core, FeeType};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use inquire::{Select, Text};
use log::error;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use wisp_core::currency::Amount;
use wisp_core::network::TransactionStatus;

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
        ); // Pass name here

        let send_receive_options = vec![
            "Send funds",
            "Receive funds",
            "Transaction history",
            "Back to wallet menu",
        ];
        let send_receive_menu_selection =
            Select::new("Send and Receive Funds", send_receive_options).prompt()?;

        // Corrected match statement: use .as_ref() to get &str from String
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
                // This will catch any unexpected selections or errors from prompt()
                error!("Send/Receive menu selection error: Invalid selection or prompt error.");
                println!("Invalid selection or prompt error.");
                pause();
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

    let amount_str = Text::new("Enter amount to send (e.g., 1 or 0.5 Wisp):") // Changed prompt for Wisp units
        .prompt()
        .context("Failed to read amount")?;
    let amount_obj = Amount::from_string_wisp(&amount_str) // Use from_string_wisp
        .context("Invalid amount entered. Please use a number, e.g., 1.23")?;

    let fee_type: FeeType =
        Select::new("Select fee type:", vec![FeeType::Fixed, FeeType::Percent]).prompt()?;

    let fee_value_str_input = Text::new(
        format!(
            "Enter fee value (for {}: e.g., 0.01 for Fixed Wisp; 0.0-100.0 for Percentage):", // Changed prompt for fee
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
            // Avoid using f64 for financial calculations to prevent precision loss.
            // Instead, parse the integer and fractional parts of the percentage string.
            let parts: Vec<&str> = fee_value_str_input.split('.').collect();
            let integer_part = parts[0]
                .parse::<u64>()
                .context("Invalid integer part of percentage")?;

            // We support up to 2 decimal places for basis points (e.g., 1.23% = 123 basis points).
            let fractional_part_str = if parts.len() > 1 { parts[1] } else { "0" };
            if fractional_part_str.len() > 2 {
                return Err(anyhow!("Percentage fee supports up to two decimal places."));
            }

            // Pad with a '0' if only one decimal place is given (e.g., "1.2" -> "20")
            let fractional_part_str_padded = format!("{:<02}", fractional_part_str);
            let fractional_part = fractional_part_str_padded
                .parse::<u64>()
                .context("Invalid fractional part of percentage")?;

            // Calculate basis points (1% = 100 basis points).
            fee_value_for_core = integer_part
                .saturating_mul(100)
                .saturating_add(fractional_part);

            if fee_value_for_core > 10_000 {
                return Err(anyhow!("Percentage fee cannot exceed 100.00%"));
            }

            if fee_value_for_core > 10_000 {
                // Cap at 100%
                fee_value_for_core = 10_000;
            }
        }
    }

    match core
        .send_funds(
            false, //TODO is_send_max is false for now, as the UI doesn't support it yet.
            recipient_public_key_str,
            amount_obj, // Pass the Amount object directly
            fee_type,
            fee_value_for_core, // Pass the u64 fee value (smallest units or basis points)
            &prompt_password("Enter your wallet password:", false)?,
            _config_path,
        )
        .await
    {
        Ok(_) => println!("✅ Funds sent successfully!"),
        Err(e) => {
            println!("\n❌ Error sending funds: {}", e);
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
            println!("\nYour public key (share this to receive funds):");
            println!("{}", wallet.public_key.fingerprint());
        }
        Err(_) => println!("⚠️ No wallet loaded."),
    }
    pause();
    Ok(())
}

struct TxDisplayItem {
    timestamp: DateTime<Utc>,  // For sorting
    display_date_time: String, // Full date and time
    tx_type: String,           // "Incoming", "Outgoing", "Sent", "Received", "Coinbase Reward"
    amount_str: String,
    counterparty_info: String, // Address or "Coinbase Reward" or "multiple recipients"
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

    println!(
        "Fetching transaction history for wallet: {}",
        wallet_public_key.fingerprint()
    );

    let mut display_items: Vec<TxDisplayItem> = Vec::new();

    let all_txs = core.transactions.read().await;
    for tx_info in all_txs.values() {
        let tx = &tx_info.transaction;
        let tx_hash = tx.txid()?;
        let is_pending = tx_info.status == TransactionStatus::Pending;
        let is_coinbase = tx.is_coinbase();

        let display_date_time = if is_pending {
            format!("{} (Pending)", Utc::now().format("%d.%m.%Y %H:%M:%S"))
        } else {
            tx_info
                .block_timestamp
                .map(|ts| ts.format("%d.%m.%Y %H:%M:%S").to_string())
                .unwrap_or_else(|| "Unknown Date/Time".to_string())
        };

        let mut value_from_us = Amount::zero();
        let mut value_to_us = Amount::zero();

        // Calculate value from us (inputs we owned)
        // To do this, we need to find the source transaction for each input.
        for input in &tx.inputs {
            // We need to look up the output this input is spending.
            // The most reliable way is to check all transactions, but this can be slow.
            // A better approach is to rely on the UTXO set at the time of the transaction,
            // but for history, we must reconstruct.
            // Let's find the source transaction in our `all_txs` map.
            if let Some(source_tx) = all_txs.get(&input.outpoint.txid) {
                if let Some(spent_output) = source_tx
                    .transaction
                    .outputs
                    .get(input.outpoint.vout as usize)
                {
                    if spent_output.pubkey == wallet_public_key {
                        value_from_us = value_from_us
                            .checked_add(spent_output.value)
                            .context("Overflow calculating value from us in transaction history")?;
                    }
                }
            }
        }

        // Calculate value to us (outputs we received)
        for output in &tx.outputs {
            if output.pubkey == wallet_public_key {
                value_to_us = value_to_us
                    .checked_add(output.value)
                    .context("value_to_us overflow")?;
            }
        }

        // If we didn't send or receive anything, it's not our transaction.
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
            // Outgoing transaction
            // We sent more than we received (net outgoing)
            let recipients: HashSet<_> = tx
                .outputs
                .iter()
                .filter(|o| o.pubkey != wallet_public_key)
                .map(|o| o.pubkey.fingerprint())
                .collect();

            // This is the amount sent to others, NOT including the fee.
            let amount_sent_to_others: Amount = tx
                .outputs
                .iter()
                .filter(|o| o.pubkey != wallet_public_key)
                .map(|o| o.value)
                .sum();

            // The fee is the difference between what we put in and what came out (to us and to others)
            let fee = value_from_us
                .checked_sub(value_to_us)
                .and_then(|v| v.checked_sub(amount_sent_to_others));

            let recipient_info = if recipients.is_empty() {
                "Self (fee only)".to_string() // Sent to ourself
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
            // Incoming or self-transfer
            // We received more than we sent (net incoming)
            let amount_received = Amount::from_smallest_unit(net_effect.abs() as u64);
            let senders: HashSet<_> = tx
                .inputs
                .iter()
                .filter_map(|i| all_txs.get(&i.outpoint.txid).map(|info| (i, info)))
                .flat_map(|(i, source_tx_info)| {
                    source_tx_info
                        .transaction
                        .outputs
                        .get(i.outpoint.vout as usize) // `i` is now in scope here
                        .into_iter()
                        .filter(|o| o.pubkey != wallet_public_key) // Don't list ourselves as sender
                        .map(|o| o.pubkey.fingerprint())
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

    // Sort transactions: pending first, then by timestamp (newest first)
    display_items.sort_by(|a, b| {
        if a.is_pending && !b.is_pending {
            std::cmp::Ordering::Less // Pending comes before confirmed
        } else if !a.is_pending && b.is_pending {
            std::cmp::Ordering::Greater // Confirmed comes after pending
        } else {
            b.timestamp.cmp(&a.timestamp) // Sort by timestamp descending (newest first)
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
            "{:<20} {:<10} {:>28}  {:<30}",
            "Date/Time", "Type", "Amount", "Counterparty/Memo"
        ); // Adjusted header width
        println!("{}", "-".repeat(95)); // Adjust length based on column widths

        let start_index = current_page * TRANSACTIONS_PER_PAGE;
        let end_index = (start_index + TRANSACTIONS_PER_PAGE).min(display_items.len());

        if display_items.is_empty() {
            // If no items at all
            println!("\nNo transactions found for this wallet.");
        } else if start_index >= display_items.len() && !display_items.is_empty() {
            // Added !display_items.is_empty() for safety
            println!("\nNo more transactions on this page."); // Fallback, should generally not be reached with correct paging
        }

        for tx_item in display_items[start_index..end_index].iter() {
            let color_code = if tx_item.is_pending {
                "\x1B[33m" // Yellow for pending (Outgoing/Incoming Unconfirmed)
            } else if tx_item.amount_str.starts_with('+') {
                "\x1B[32m" // Green for positive (Received/Coinbase)
            } else {
                "\x1B[31m" // Red for negative (Sent)
            };
            let reset_color = "\x1B[0m";

            // Print the main transaction line with colors
            println!(
                "{}{:<20} {:<10} {:>28}  {:<30}{}", // Adjusted widths
                color_code,
                tx_item.display_date_time,
                tx_item.tx_type,
                tx_item.amount_str,
                tx_item.counterparty_info,
                reset_color
            );
            println!(
                "  \x1B[90mLink: \x1B[4mhttps://localhost:8000/transaction/{}\x1B[0m", // TODO: Change to wisp.com
                tx_item.tx_hash.to_string()
            );
            println!();
        }
        println!("{}", "-".repeat(95));

        let mut page_options = Vec::new();
        if current_page > 0 {
            page_options.push("Previous Page");
        }
        if current_page < total_pages.saturating_sub(1) {
            // Use saturating_sub to prevent underflow if total_pages is 0 or 1
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
            _ => {} // Should not happen
        }
    }
}
