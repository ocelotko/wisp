use crate::utils::{clear_terminal, display_heading_with_wallet, pause, prompt_password};
use crate::wallet::core::Core;
use anyhow::{Context, Result};
use inquire::Select;
use log::error;
use std::path::PathBuf;
use std::sync::Arc;

pub async fn security_and_backup(
    core: Arc<Core>,
    config_path: &PathBuf,
) -> Result<(), anyhow::Error> {
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
        let security_options = vec![
            "Backup wallet",
            "Export private key",
            "Change wallet password",
            "Back to wallet menu",
        ];
        let security_menu_selection =
            Select::new("Security and Backup", security_options).prompt()?;

        match security_menu_selection.as_ref() {
            "Backup wallet" => {
                println!("This feature is currently disabled.");
                pause();
            }
            "Export private key" => {
                if let Err(e) =
                    export_private_key_command(Arc::clone(&core), config_path.clone()).await
                {
                    error!("Export private key failed: {}", e);
                    println!("\nError: {}", e);
                    pause();
                }
            }
            "Change wallet password" => {
                if let Err(e) = change_wallet_password_prompt(core.clone()).await {
                    error!("Change wallet password failed: {}", e);
                    println!("\nError: {}", e);
                    pause();
                } else {
                    println!("Password changed successfully.");
                    pause();
                }
            }
            "Back to wallet menu" => return Ok(()),
            _ => {
                error!("Security menu selection error: Invalid selection or prompt error.");
                println!("Invalid selection or prompt error.");
                pause();
            }
        }
    }
}

async fn export_private_key_command(core: Arc<Core>, _config_path: PathBuf) -> Result<()> {
    clear_terminal();
    let current_wallet_name = core.get_current_wallet().await?.name;
    let balance_value = core.get_total_balance().await;
    display_heading_with_wallet(Some(&current_wallet_name), balance_value.ok());
    println!("--- Export Private Key ---");
    println!("WARNING: Your private key grants full control over your funds. Only export if you understand the risks and keep it highly secure!");
    println!("It is NOT recommended to share this key or store it insecurely.");

    let password = prompt_password(
        &format!("Enter password for wallet '{}':", current_wallet_name),
        true,
    )?;

    match core.decrypt_current_wallet_private_key(&password).await {
        Ok(private_key) => {
            let private_key_bytes = private_key.0.to_bytes();
            let private_key_hex = hex::encode(private_key_bytes);
            let public_key_hex = private_key.public_key().fingerprint();

            println!("\n--- YOUR PRIVATE KEY (HEX) ---");
            println!("{}", private_key_hex);
            println!("------------------------------");
            println!("Corresponding Public Key (HEX): {}", public_key_hex);
            println!("\nCopy this private key carefully. Do not share it!");
        }
        Err(e) => {
            println!("⚠️ Failed to decrypt private key: {}", e);
            return Err(e);
        }
    }
    pause();
    Ok(())
}

async fn change_wallet_password_prompt(core: Arc<Core>) -> Result<(), anyhow::Error> {
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

    let current_password = prompt_password("Enter your current password:", true)?;
    let new_password = prompt_password("Enter your new password:", true)?;

    match core
        .change_wallet_password(&current_password, &new_password)
        .await
    {
        Ok(_) => {
            pause();
        }
        Err(e) => return Err(e).context("Failed to change wallet password"),
    };
    Ok(())
}
