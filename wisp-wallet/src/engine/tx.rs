use crate::engine::session::Core;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use k256::ecdsa::signature::Signer;
use std::collections::HashSet;
use std::path::PathBuf;
use wisp_core::{
    currency::Amount,
    network::{Message, TransactionStatus, WalletMessage, WalletTransactionInfo},
    sha256::Hash,
    transactions::{OutPoint, Script, Transaction, TransactionInput, TransactionOutput},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeeType {
    Fixed,
    Percent,
}

impl std::fmt::Display for FeeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeeType::Fixed => write!(f, "Fixed Amount"),
            FeeType::Percent => write!(f, "Percentage"),
        }
    }
}

pub struct TxHistoryItem {
    pub txid: Hash,
    pub timestamp: DateTime<Utc>,
    pub tx_type: String,
    pub amount: Amount,
    pub counterparty: String,
    pub is_pending: bool,
}

pub struct WalletSummary {
    pub confirmed_balance: Amount,
    pub pending_net_change: i128,
    pub total_balance: Amount,
    pub utxo_count: usize,
    pub pending_tx_count: usize,
}

impl Core {
    #[allow(clippy::too_many_arguments)]
    pub async fn send_funds(
        &self,
        is_send_max: bool,
        recipient_address_str: String,
        amount_str: &str,
        fee_type: FeeType,
        fee_value_str: &str,
        password: &str,
        _config_path: &PathBuf,
    ) -> Result<()> {
        let current_wallet = self.get_current_wallet().await?;
        let sender_private_key = self
            .decrypt_current_private_key(password)
            .await
            .context("Incorrect wallet password or decryption failed")?;

        let recipient_script = wisp_core::address::Address::decode(&recipient_address_str)
            .context("Invalid recipient address format")?;

        let amount_to_send = if is_send_max {
            Amount::zero()
        } else {
            Amount::from_string_wisp(amount_str).context("Invalid amount format")?
        };

        let fee_value_raw = match fee_type {
            FeeType::Fixed => Amount::from_string_wisp(fee_value_str)
                .context("Invalid fixed fee format")?
                .as_smallest_unit(),
            FeeType::Percent => {
                let parts: Vec<&str> = fee_value_str.split('.').collect();
                let integer_part = parts[0]
                    .parse::<u64>()
                    .context("Invalid percentage integer")?;
                let fractional_part = if parts.len() > 1 {
                    let f = parts[1];
                    if f.len() > 2 {
                        return Err(anyhow!("Percent supports 2 decimal places"));
                    }
                    let mut val = f.parse::<u64>().context("Invalid percentage fraction")?;
                    if f.len() == 1 {
                        val *= 10;
                    }
                    val
                } else {
                    0
                };

                integer_part
                    .saturating_mul(100)
                    .saturating_add(fractional_part)
                    .min(10_000)
            }
        };

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
            .context("Total overflow")?;
        let spendable_utxos = self.utxos.read().await.clone();

        let mut selected_inputs = Vec::new();
        let mut current_input_sum = Amount::zero();
        let mut all_utxos: Vec<_> = spendable_utxos.iter().collect();
        all_utxos.sort_by_key(|(_, o)| o.value);

        for (op, utxo) in all_utxos {
            if is_send_max || current_input_sum < total_required {
                selected_inputs.push(TransactionInput {
                    outpoint: *op,
                    public_key: Some(sender_private_key.public_key()),
                    ..Default::default()
                });
                current_input_sum += utxo.value;
            }
        }

        let (final_amount, transaction_fee) = if is_send_max {
            let fee = match fee_type {
                FeeType::Fixed => Amount::from_smallest_unit(fee_value_raw),
                FeeType::Percent => {
                    Amount::from_smallest_unit(current_input_sum.0 * fee_value_raw / 10_000)
                }
            };
            (
                current_input_sum
                    .checked_sub(fee)
                    .context("Send max underflow")?,
                fee,
            )
        } else {
            (amount_to_send, intended_fee)
        };

        let mut outputs = vec![TransactionOutput {
            value: final_amount,
            script: recipient_script,
        }];
        let change_amount = current_input_sum
            .checked_sub(final_amount + transaction_fee)
            .context("Change error")?;

        if change_amount > Amount::zero() {
            outputs.push(TransactionOutput {
                value: change_amount,
                script: Script::new_aurora(&current_wallet.public_key),
            });
        }

        let mut new_tx = Transaction {
            inputs: selected_inputs,
            outputs,
        };
        let txid_for_signing = new_tx.txid()?;

        for input in &mut new_tx.inputs {
            let sig = wisp_core::signatures::Signature(
                sender_private_key.0.sign(&txid_for_signing.as_bytes()),
            );
            input.signature = Some(sig);
        }

        let final_txid = new_tx.txid()?;
        let mut stream_guard = self.get_connected_stream().await?;
        let stream = stream_guard.as_mut().ok_or_else(|| anyhow!("No stream"))?;

        let msg = Message::Wallet(WalletMessage::SubmitTransaction(new_tx.clone()));
        msg.send_async(stream).await?;

        let response = tokio::time::timeout(
            self.get_node_response_timeout().await,
            Message::receive_async(stream),
        )
        .await??;

        match response {
            Message::Wallet(WalletMessage::TransactionAcceptedConfirmation) => {
                let mut txs_guard = self.transactions.write().await;
                txs_guard.insert(
                    final_txid,
                    WalletTransactionInfo {
                        transaction: new_tx.clone(),
                        status: TransactionStatus::Pending,
                        block_timestamp: Some(Utc::now()),
                        block_index: None,
                    },
                );
                let mut utxos_guard = self.utxos.write().await;
                for input in &new_tx.inputs {
                    utxos_guard.remove(&input.outpoint);
                }
                for (vout, output) in new_tx.outputs.iter().enumerate() {
                    if current_wallet.is_script_relevant(&output.script) {
                        utxos_guard.insert(
                            OutPoint {
                                txid: final_txid,
                                vout: vout as u32,
                            },
                            output.clone(),
                        );
                    }
                }
                Ok(())
            }
            Message::Wallet(WalletMessage::TransactionRejected(_, reason)) => {
                Err(anyhow!("Rejected: {}", reason))
            }
            _ => Err(anyhow!("Unexpected response")),
        }
    }

    /// Logic for calculating the net effect and type of every transaction in the wallet.
    pub async fn get_history(&self) -> Result<Vec<TxHistoryItem>> {
        let current_wallet = self.get_current_wallet().await?;

        let all_txs = self.transactions.read().await;
        let mut items = Vec::new();

        for tx_info in all_txs.values() {
            let tx = &tx_info.transaction;
            let is_coinbase = tx.is_coinbase();
            let mut value_from_us = Amount::zero();
            let mut value_to_us = Amount::zero();

            // Calculate outflow
            for input in &tx.inputs {
                if let Some(source_tx) = all_txs.get(&input.outpoint.txid) {
                    if let Some(output) = source_tx
                        .transaction
                        .outputs
                        .get(input.outpoint.vout as usize)
                    {
                        if current_wallet.is_script_relevant(&output.script) {
                            value_from_us += output.value;
                        }
                    }
                }
            }

            // Calculate inflow
            for output in &tx.outputs {
                if current_wallet.is_script_relevant(&output.script) {
                    value_to_us += output.value;
                }
            }

            if value_from_us == Amount::zero() && value_to_us == Amount::zero() {
                continue;
            }

            let net_val = (value_to_us.0 as i128) - (value_from_us.0 as i128);

            let (tx_type, amount, counterparty) = if is_coinbase {
                (
                    "Coinbase".to_string(),
                    value_to_us,
                    "Network Reward".to_string(),
                )
            } else if net_val < 0 {
                let recipients: HashSet<_> = tx
                    .outputs
                    .iter()
                    .filter(|o| !current_wallet.is_script_relevant(&o.script))
                    .map(|o| wisp_core::address::Address::encode(&o.script))
                    .collect();

                let amount_to_others: Amount = tx
                    .outputs
                    .iter()
                    .filter(|o| !current_wallet.is_script_relevant(&o.script))
                    .map(|o| o.value)
                    .sum();

                let counter_info = if recipients.is_empty() {
                    "Self (fee only)".to_string()
                } else if recipients.len() == 1 {
                    recipients.iter().next().unwrap().clone()
                } else {
                    format!("{} recipients", recipients.len())
                };

                ("Sent".to_string(), amount_to_others, counter_info)
            } else {
                let senders: HashSet<_> = tx
                    .inputs
                    .iter()
                    .filter_map(|i| all_txs.get(&i.outpoint.txid).map(|info| (i, info)))
                    .flat_map(|(i, source)| {
                        source
                            .transaction
                            .outputs
                            .get(i.outpoint.vout as usize)
                            .into_iter()
                            .filter(|o| !current_wallet.is_script_relevant(&o.script))
                            .map(|o| wisp_core::address::Address::encode(&o.script))
                    })
                    .collect();

                let counter_info = if senders.is_empty() {
                    "Unknown".to_string()
                } else if senders.len() == 1 {
                    senders.iter().next().unwrap().clone()
                } else {
                    format!("{} senders", senders.len())
                };

                (
                    "Received".to_string(),
                    Amount::from_smallest_unit(net_val.abs() as u64),
                    counter_info,
                )
            };

            items.push(TxHistoryItem {
                txid: tx.txid()?,
                timestamp: tx_info.block_timestamp.unwrap_or_else(Utc::now),
                tx_type,
                amount,
                counterparty,
                is_pending: tx_info.status == TransactionStatus::Pending,
            });
        }

        items.sort_by(|a, b| {
            b.is_pending
                .cmp(&a.is_pending)
                .then(b.timestamp.cmp(&a.timestamp))
        });
        Ok(items)
    }

    /// Centralized logic for wallet balance and state summary.
    pub async fn get_wallet_summary(&self) -> Result<WalletSummary> {
        let utxos_guard = self.utxos.read().await;
        let transactions_guard = self.transactions.read().await;
        let current_wallet = self.get_current_wallet().await?;

        let confirmed_balance: Amount = utxos_guard.values().map(|o| o.value).sum();
        let mut pending_net_change: i128 = 0;
        let mut pending_tx_count = 0;

        for tx_info in transactions_guard.values() {
            if tx_info.status == TransactionStatus::Pending {
                pending_tx_count += 1;
                for input in &tx_info.transaction.inputs {
                    if let Some(spent) = utxos_guard.get(&input.outpoint) {
                        if current_wallet.is_script_relevant(&spent.script) {
                            pending_net_change -= spent.value.0 as i128;
                        }
                    }
                }
                for output in &tx_info.transaction.outputs {
                    if current_wallet.is_script_relevant(&output.script) {
                        pending_net_change += output.value.0 as i128;
                    }
                }
            }
        }

        Ok(WalletSummary {
            confirmed_balance,
            pending_net_change,
            total_balance: Amount::from_smallest_unit(
                (confirmed_balance.0 as i128 + pending_net_change).max(0) as u64,
            ),
            utxo_count: utxos_guard.len(),
            pending_tx_count,
        })
    }
}
