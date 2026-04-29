use std::io::{self, Write};
use wisp_core::currency::Amount;

use crate::{ui::clear_terminal, utils::display_heading};

pub fn display_heading_with_wallet(wallet_name: Option<&str>, balance: Option<Amount>) {
    clear_terminal();
    display_heading();
    match wallet_name {
        Some(name) => {
            println!("\x1B[1mActive wallet:\x1B[0m {}", name);
            if let Some(amt) = balance {
                println!("\x1B[1mBalance:\x1B[0m {}", amt.to_string_wisp());
            } else {
                println!("\x1B[1mBalance:\x1B[0m (N/A)");
            }
        }
        None => println!("No wallet currently loaded."),
    }
    println!();
}

pub fn display_seed_phrase(phrase: &str) {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    let word_count = words.len();

    if ![12, 24].contains(&word_count) {
        println!("{}", phrase);
        return;
    }

    let (num_cols, num_rows) = if word_count == 12 { (3, 4) } else { (4, 6) };

    let max_len = words.iter().map(|w| w.len()).max().unwrap_or(0);

    for r in 0..num_rows {
        let mut line = String::new();
        for c in 0..num_cols {
            let index = c * num_rows + r;
            if index < word_count {
                let word = words[index];
                let entry = format!("{:>2}. {}", index + 1, word);
                line.push_str(&format!("{:<width$}", entry, width = max_len + 8));
            }
        }
        println!("{}", line);
    }
}

pub fn pause() {
    print!("\nPress Enter to continue...");
    io::stdout().flush().unwrap();
    let _ = io::stdin().read_line(&mut String::new());
}
