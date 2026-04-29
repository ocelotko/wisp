use anyhow::Result;
use inquire::Select;
use std::sync::Arc;

use crate::{
    engine::session::Core,
    ui::{
        clear_terminal,
        views::layout::{display_heading_with_wallet, pause},
    },
};

const TRANSACTIONS_PER_PAGE: usize = 5;

pub(crate) async fn transaction_history_view(core: Arc<Core>) -> Result<(), anyhow::Error> {
    clear_terminal();
    let summary = core.get_wallet_summary().await.ok();
    let wallet_name = core.get_current_wallet().await.ok().map(|w| w.name);
    display_heading_with_wallet(wallet_name.as_deref(), summary.map(|s| s.total_balance));

    println!("Fetching transaction history...");
    let display_items = core.get_history().await?;

    // --- Pagination and Display Loop ---
    let total_pages = (display_items.len() + TRANSACTIONS_PER_PAGE - 1) / TRANSACTIONS_PER_PAGE;
    let mut current_page = 0;

    loop {
        clear_terminal();
        let summary = core.get_wallet_summary().await.ok();
        let wallet_name = core.get_current_wallet().await.ok().map(|w| w.name);
        display_heading_with_wallet(wallet_name.as_deref(), summary.map(|s| s.total_balance));

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
            let amount_display = tx_item.amount.to_string_wisp();
            let color_code = if tx_item.is_pending {
                "\x1B[33m"
            } else if tx_item.tx_type == "Received" || tx_item.tx_type == "Coinbase" {
                "\x1B[32m"
            } else {
                "\x1B[31m"
            };
            let reset_color = "\x1B[0m";

            println!(
                "{}{:<20} {:<10} {:<10} {:>25}  {:<30}{}",
                color_code,
                tx_item.timestamp.format("%Y-%m-%d %H:%M:%S"),
                status_str,
                tx_item.tx_type,
                amount_display,
                tx_item.counterparty,
                reset_color
            );
            println!(
                "  \x1B[90mhttp://localhost:8000/transaction/{}\x1B[0m",
                tx_item.txid.to_string()
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
