use crate::engine::session::Core;
use crate::ui::clear_terminal;
use crate::ui::views::layout::{display_heading_with_wallet, display_seed_phrase, pause};
use crate::utils::prompt_password;
use anyhow::Result;
use inquire::Select;
use std::path::PathBuf;
use std::sync::Arc;

pub async fn security_and_backup_loop(core: Arc<Core>, _config_path: &PathBuf) -> Result<()> {
    loop {
        clear_terminal();
        let summary = core.get_wallet_summary().await.ok();
        let wallet = core.get_current_wallet().await?;
        display_heading_with_wallet(Some(&wallet.name), summary.map(|s| s.total_balance));

        let options = vec![
            "Export private key",
            "View seed phrase",
            "Change wallet password",
            "Back to wallet menu",
        ];
        let selection = Select::new("Security and Backup", options).prompt()?;

        match selection {
            "Export private key" => {
                let pwd = prompt_password("Enter password to decrypt key:", false)?;
                match core.decrypt_current_private_key(&pwd).await {
                    Ok(pk) => {
                        println!("\n--- YOUR PRIVATE KEY (HEX) ---");
                        println!("{}", pk.to_hex());
                        println!("------------------------------");
                        println!(
                            "Corresponding Public Key: {}",
                            pk.public_key().fingerprint()
                        );
                        println!("\nWARNING: Keep this key secret. Anyone with this key can spend your funds.");
                    }
                    Err(_) => println!("Error: Incorrect password."),
                }
                pause();
            }
            "View seed phrase" => {
                let pwd = prompt_password("Enter password to view seed phrase:", false)?;
                match core.export_seed_phrase(&pwd).await {
                    Ok(phrase) => {
                        println!("\n--- YOUR SEED PHRASE ---");
                        display_seed_phrase(&phrase);
                        println!("------------------------");
                    }
                    Err(e) => println!("Error: {}", e),
                }
                pause();
            }
            "Change wallet password" => {
                let current = prompt_password("Enter current password:", false)?;
                let new = prompt_password("Enter new password:", true)?;

                match core.change_wallet_password(&current, &new).await {
                    Ok(_) => println!("Password updated and wallet file re-encrypted."),
                    Err(e) => println!("Failed to change password: {}", e),
                }
                pause();
            }
            "Back to wallet menu" => return Ok(()),
            _ => {}
        }
    }
}
