use anyhow::{anyhow, Result};
use inquire::Password;
use std::io::{self, Write};
use wisp_core::currency::Amount;

pub fn clear_terminal() {
    print!("\x1B[2J\x1B[1;1H");
    io::stdout().flush().unwrap();
}

pub fn display_heading() {
    println!("\n\x1B[1m\x1B[96mWisp\x1B[0m");
}

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

pub fn check_terminal_size() -> bool {
    if let Some((width, height)) = termion::terminal_size().ok() {
        if width < 80 || height < 20 {
            println!("Terminal too small. Please resize to at least 80x20.");
            return false;
        }
    }
    true
}

pub fn prompt_password(prompt: &str, require_confirmation: bool) -> Result<String> {
    let mut password_prompt = Password::new(prompt);
    if !require_confirmation {
        password_prompt = password_prompt.without_confirmation();
    }

    password_prompt.prompt().map_err(|e| anyhow!(e))
}

pub fn pause() {
    print!("\nPress Enter to continue...");
    io::stdout().flush().unwrap();
    let _ = io::stdin().read_line(&mut String::new());
}

pub fn exit_program() -> ! {
    println!("\nExiting Wisp Wallet. Goodbye!");
    std::process::exit(0);
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
        let max_len = words.iter().map(|w| w.len()).max().unwrap_or(0);
    }
}
