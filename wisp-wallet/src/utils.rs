use anyhow::{anyhow, Result};
use inquire::Password;

pub fn display_heading() {
    println!("\n\x1B[1m\x1B[96mWisp\x1B[0m");
}

pub fn prompt_password(prompt: &str, require_confirmation: bool) -> Result<String> {
    let mut password_prompt = Password::new(prompt);
    if !require_confirmation {
        password_prompt = password_prompt.without_confirmation();
    }

    password_prompt.prompt().map_err(|e| anyhow!(e))
}

pub fn exit_program() -> ! {
    println!("\nExiting Wisp Wallet. Goodbye!");
    std::process::exit(0);
}
