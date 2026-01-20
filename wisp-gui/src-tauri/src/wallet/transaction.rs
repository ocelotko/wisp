use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use k256::ecdsa::signature::Signer;
use log::{info, warn};

use wisp_core::{
    currency::Amount,
    network::{Message, TransactionStatus, WalletMessage, WalletTransactionInfo},
    signatures::PublicKey,
    transactions::{OutPoint, Transaction, TransactionInput, TransactionOutput},
};

use crate::wallet::{
    account::{decrypt_current_wallet_private_key, get_current_wallet},
    core::Core,
    types::FeeType,
};

pub async fn get_total_balance(core: &Core) -> Result<Amount> {
    let utxos_guard = core.utxos.read().await;
    let transactions_guard = core.transactions.read().await;
    let wallet_public_key = get_current_wallet(core).await?.public_key;
    let confirmed_balance: Amount = utxos_guard.values().map(|output| output.value).sum();

    let mut pending_net_change: i128 = 0;
    for tx_info in transactions_guard.values() {
        if tx_info.status == TransactionStatus::Pending {
            let tx = &tx_info.transaction;

            for input in &tx.inputs {
                if let Some(spent_utxo) = utxos_guard.get(&input.outpoint) {
                    if spent_utxo.pubkey == wallet_public_key {
                        pending_net_change -= spent_utxo.value.as_smallest_unit() as i128;
                    }
                }
            }

            for output in &tx.outputs {
                if output.pubkey == wallet_public_key {
                    pending_net_change += output.value.as_smallest_unit() as i128;
                }
            }
        }
    }

    let total_balance_units =
        (confirmed_balance.as_smallest_unit() as i128 + pending_net_change).max(0) as u64;

    Ok(Amount::from_smallest_unit(total_balance_units))
}

#[allow(clippy::too_many_arguments)]
pub async fn send_funds(
    core: &Core,
    is_send_max: bool,
    recipient_public_key_str: String,
    amount_to_send: Amount,
    fee_type: FeeType,
    fee_value_raw: u64,
    password: &str,
    _config_path: &PathBuf,
) -> Result<()> {
    let current_wallet = get_current_wallet(core).await?;
    let sender_private_key = decrypt_current_wallet_private_key(core, password)
        .await
        .context("Incorrect wallet password or decryption failed")?;

    let recipient_public_key = recipient_public_key_str
        .parse::<PublicKey>()
        .context("Invalid recipient public key format")?;

    let intended_fee = if !is_send_max {
        match fee_type {
            FeeType::Fixed => Amount::from_smallest_unit(fee_value_raw),
            FeeType::Percent => Amount::from_smallest_unit(
                amount_to_send.as_smallest_unit() * fee_value_raw / 10_000,
            ),
        }
    } else {
        Amount::zero()
    };

    let total_required = amount_to_send
        .checked_add(intended_fee)
        .context("Total required amount overflow")?;

    let spendable_utxos = core.utxos.read().await.clone();
    info!(
        "Creating transaction with {} locally known UTXOs.",
        spendable_utxos.len()
    );

    let mut selected_inputs: Vec<TransactionInput> = Vec::new();
    let mut current_input_sum = Amount::zero();

    let mut all_spendable_utxos: Vec<_> = spendable_utxos.iter().collect();
    all_spendable_utxos.sort_by_key(|(_, output)| output.value.as_smallest_unit());

    for (outpoint, utxo_output) in all_spendable_utxos {
        if is_send_max || (current_input_sum < total_required) {
            selected_inputs.push(TransactionInput {
                outpoint: *outpoint,
                signature: None,
                coinbase_data: None,
            });
            current_input_sum = current_input_sum
                .checked_add(utxo_output.value)
                .context("Input sum overflow")?;
        } else {
            break;
        }
    }

    if !is_send_max && current_input_sum < total_required {
        return Err(anyhow!(
            "Insufficient funds. Available: {}, Required: {}",
            current_input_sum,
            total_required
        ));
    }

    let (final_amount_to_send, transaction_fee) = if is_send_max {
        let fee = match fee_type {
            FeeType::Fixed => Amount::from_smallest_unit(fee_value_raw),
            FeeType::Percent => Amount::from_smallest_unit(
                current_input_sum.as_smallest_unit() * fee_value_raw / 10_000,
            ),
        };
        let amount = current_input_sum
            .checked_sub(fee)
            .context("Fee calculation underflow for send max")?;
        (amount, fee)
    } else {
        (amount_to_send, intended_fee)
    };

    let mut outputs: Vec<TransactionOutput> = Vec::new();
    outputs.push(TransactionOutput {
        value: final_amount_to_send,
        pubkey: recipient_public_key,
    });

    let total_to_distribute = final_amount_to_send
        .checked_add(transaction_fee)
        .context("Total amount + fee calculation overflow")?;
    let change_amount = current_input_sum
        .checked_sub(total_to_distribute)
        .context("Change calculation underflow (input sum < amount + fee)")?;

    if change_amount > Amount::zero() {
        outputs.push(TransactionOutput {
            value: change_amount,
            pubkey: current_wallet.public_key,
        });
    }

    let mut new_transaction = Transaction {
        inputs: selected_inputs,
        outputs,
    };

    let tx_hash_for_signing = new_transaction.txid()?;

    for input in &mut new_transaction.inputs {
        let signature = wisp_core::signatures::Signature(
            sender_private_key
                .0
                .sign(&tx_hash_for_signing.as_bytes()[..]),
        );
        input.signature = Some(signature.clone());
    }

    let final_txid = new_transaction.txid()?;
    let mut stream_guard = core.get_connected_stream().await?;
    let stream_ref = stream_guard
        .as_mut()
        .expect("Expected an active TCP stream after connection attempt");
    let response_timeout = core.get_node_response_timeout().await;

    let submit_tx_msg = Message::Wallet(WalletMessage::SubmitTransaction(new_transaction.clone()));
    if let Err(e) = submit_tx_msg.send_async(stream_ref).await {
        *stream_guard = None;
        return Err(anyhow!("Failed to send SubmitTransaction message: {}", e));
    }

    let confirmation_response =
        match tokio::time::timeout(response_timeout, Message::receive_async(stream_ref)).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(e)) => {
                *stream_guard = None;
                return Err(anyhow!(
                    "Failed to receive SubmitTransaction confirmation: {}",
                    e
                ));
            }
            Err(_) => {
                *stream_guard = None;
                return Err(anyhow!(
                    "Timeout waiting for SubmitTransaction confirmation"
                ));
            }
        };

    match confirmation_response {
        Message::Wallet(WalletMessage::TransactionAcceptedConfirmation) => {
            info!("Transaction submitted and accepted by node.");
            println!(
                "Transaction submitted, waiting for confirmation. Hash: {}",
                final_txid
            );

            let mut transactions_guard = core.transactions.write().await;
            let tx_info = WalletTransactionInfo {
                transaction: new_transaction.clone(),
                status: TransactionStatus::Pending,
                block_timestamp: Some(Utc::now()),
                block_index: None,
            };
            transactions_guard.insert(final_txid, tx_info);
            drop(transactions_guard);

            let mut utxos_guard = core.utxos.write().await;
            for input in &new_transaction.inputs {
                utxos_guard.remove(&input.outpoint);
            }
            for (vout, output) in new_transaction.outputs.iter().enumerate() {
                if output.pubkey == current_wallet.public_key {
                    utxos_guard.insert(
                        OutPoint {
                            txid: final_txid,
                            vout: vout as u32,
                        },
                        output.clone(),
                    );
                }
            }
        }
        Message::Wallet(WalletMessage::TransactionRejected(hash, reason)) => {
            warn!("Transaction rejected by node: {} - {}", hash, reason);
            return Err(anyhow!("Transaction rejected by node: {}", reason));
        }
        other => {
            *stream_guard = None;
            warn!(
                "Unexpected response after submitting transaction: {:?}",
                other
            );
            return Err(anyhow!(
                "Unexpected response after submitting transaction: {:?}",
                other
            ));
        }
    }
    Ok(())
}
