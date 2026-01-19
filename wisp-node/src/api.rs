use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    Router,
};
use serde::Deserialize;
use serde::Serialize;
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};
use wisp_core::{
    blockchain::Block, currency::Amount, sha256::Hash, signatures::PublicKey,
    utils::calculate_block_reward, DAA_WINDOW, HALVING_INTERVAL, IDEAL_BLOCK_TIME,
};

#[derive(Serialize, Clone)]
struct ApiBlock {
    version: u32,
    height: u64,
    #[serde(with = "serde_hash_str")]
    hash: Hash,
    timestamp: i64,
    transactions: Vec<ApiTransactionSummary>,
    size: usize,
    nonce: u64,
    difficulty: String,
    #[serde(with = "serde_hash_str")]
    previous_hash: Hash,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_to_mine_secs: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mined_by: Option<String>,
}

#[derive(Serialize, Clone)]
struct ApiTransactionSummary {
    #[serde(with = "serde_hash_str")]
    hash: Hash,
    block_height: Option<u64>,
    input_count: usize,
    output_count: usize,
    is_coinbase: bool,
    total_output_wisp: String,
    timestamp: i64,
}

#[derive(Serialize, Clone)]
struct ApiTransactionInput {
    outpoint: String,
    signature: Option<String>,
}

#[derive(Serialize, Clone)]
struct ApiTransactionOutput {
    value: String,
    #[serde(with = "serde_pubkey_str")]
    pubkey: PublicKey,
}

#[derive(Serialize, Clone)]
struct ApiTransactionDetail {
    #[serde(with = "serde_hash_str")]
    hash: Hash,
    block_height: Option<u64>,
    timestamp: i64,
    inputs: Vec<ApiTransactionInput>,
    outputs: Vec<ApiTransactionOutput>,
    fee_or_reward: String,
    total_output_wisp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    coinbase_message: Option<String>,
}

#[derive(Serialize)]
struct MempoolInfo {
    transactions: Vec<ApiTransactionSummary>,
    count: usize,
}

#[derive(Deserialize)]
pub struct Pagination {
    page: Option<u64>,
    limit: Option<u64>,
}

#[derive(Serialize)]
struct PaginatedBlocksResponse {
    data: Vec<ApiBlock>,
    page: u64,
    limit: u64,
    total_blocks: u64,
}

#[derive(Serialize)]
struct PaginatedTransactionsResponse {
    data: Vec<ApiTransactionSummary>,
    page: u64,
    limit: u64,
    total_transactions: u64,
}

#[derive(Serialize)]
struct ApiNetworkVitals {
    current_height: u64,
    difficulty: String,
    avg_block_time_secs: f64,
    reward_per_block_wisp: String,
    blockchain_size_bytes: u64,
    next_halving_in_blocks: u64,
    mempool_size: usize,
    #[serde(with = "serde_str")]
    current_target: wisp_core::U256,
    network_hashrate_hps: f64,
    total_transactions: u64,
}

impl From<Block> for ApiBlock {
    fn from(block: Block) -> Self {
        // Use the bincode 2.x API for serialization.
        let block_size = bincode::encode_to_vec(&block, bincode::config::standard())
            .map(|v| v.len())
            .unwrap_or(0);

        let transactions_summary = block
            .transactions
            .iter()
            .map(|tx| {
                let total_output: Amount = tx.outputs.iter().map(|o| o.value).sum();
                ApiTransactionSummary {
                    hash: tx.txid().unwrap_or_default(),
                    block_height: Some(block.index),
                    input_count: tx.inputs.len(),
                    output_count: tx.outputs.len(),
                    is_coinbase: tx.is_coinbase(),
                    total_output_wisp: total_output.to_string_wisp(),
                    timestamp: block.timestamp.timestamp(),
                }
            })
            .collect();

        let difficulty_str = if block.target.is_zero() {
            "inf".to_string()
        } else {
            let scaling_factor = wisp_core::U256::from(1_000_000u64);
            let max_target = wisp_core::MAX_TARGET;

            if let Some(scaled_max) = max_target.checked_mul(scaling_factor) {
                let difficulty_scaled = scaled_max / block.target;
                format!("{:.2}", difficulty_scaled.as_u64() as f64 / 1_000_000.0)
            } else {
                "inf".to_string()
            }
        };

        ApiBlock {
            version: block.version,
            height: block.index,
            hash: block.id().unwrap_or_default(),
            timestamp: block.timestamp.timestamp(),
            difficulty: difficulty_str,
            transactions: transactions_summary,
            size: block_size,
            nonce: block.nonce,
            previous_hash: block.previous_hash,
            time_to_mine_secs: None,
            mined_by: block
                .transactions
                .first() // The coinbase transaction is always first
                .filter(|tx| tx.is_coinbase())
                .and_then(|coinbase_tx| {
                    coinbase_tx
                        .outputs
                        .first()
                        .map(|output| output.pubkey.fingerprint())
                }),
        }
    }
}

pub fn app_router(blockchain_state: Arc<RwLock<wisp_core::blockchain::Blockchain>>) -> Router {
    let cors = CorsLayer::new().allow_origin(Any);
    use axum::routing::get;

    Router::new()
        .route("/api/v1/block/height/{height}", get(get_block_by_height))
        .route("/api/v1/block/hash/{hash}", get(get_block_by_hash))
        .route("/api/v1/blocks", get(get_blocks_paginated))
        .route("/api/v1/transactions/recent", get(get_recent_transactions))
        .route("/api/v1/transactions", get(get_transactions_paginated))
        .route("/api/v1/network/vitals", get(get_network_vitals))
        .route("/api/v1/transaction/{hash}", get(get_transaction_by_hash))
        .layer(cors)
        .with_state(blockchain_state)
}

pub async fn run_api_server(blockchain: Arc<RwLock<wisp_core::blockchain::Blockchain>>) {
    let app = app_router(blockchain);
    let addr = SocketAddr::from(([0, 0, 0, 0], 3001));
    log::info!("API server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn get_network_vitals(
    State(blockchain_lock): State<Arc<RwLock<wisp_core::blockchain::Blockchain>>>,
) -> impl IntoResponse {
    let blockchain = blockchain_lock.read().await;

    let current_height = match blockchain.block_height() {
        Ok(h) => h,
        Err(e) => {
            log::error!("Failed to get chain height for vitals: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to get chain height",
            )
                .into_response();
        }
    };

    let current_target = blockchain.get_target();
    let difficulty = if current_target.is_zero() {
        "inf".to_string()
    } else {
        let scaling_factor = wisp_core::U256::from(1_000_000u64);
        let max_target = wisp_core::MAX_TARGET;

        if let Some(scaled_max) = max_target.checked_mul(scaling_factor) {
            let difficulty_scaled = scaled_max / current_target;
            format!("{:.2}", difficulty_scaled.as_u64() as f64 / 1_000_000.0)
        } else {
            "inf".to_string()
        }
    };

    let avg_block_time_secs = if current_height > 0 {
        // If the chain is mature enough, use the full DAA window for the average.
        // Otherwise, average over the blocks that do exist (from genesis to current).
        let (start_block_index, window_size) = if current_height >= DAA_WINDOW as u64 {
            (current_height - (DAA_WINDOW - 1) as u64, DAA_WINDOW)
        } else {
            (0, (current_height + 1) as usize)
        };

        if let (Ok(Some(first_block)), Ok(Some(last_block))) = (
            blockchain.get_block_by_index(start_block_index),
            blockchain.get_block_by_index(current_height),
        ) {
            let actual_timespan =
                last_block.timestamp.timestamp() - first_block.timestamp.timestamp();
            // For a window of N blocks, there are N-1 intervals.
            let num_intervals = (window_size - 1).max(1); // Avoid division by zero if only 1 block exists
            (actual_timespan as f64 / num_intervals as f64).max(1.0) // Ensure avg time is at least 1.0
        } else {
            // Fallback if blocks can't be fetched, which is unlikely for a valid chain.
            IDEAL_BLOCK_TIME as f64
        }
    } else {
        IDEAL_BLOCK_TIME as f64
    };

    let reward_per_block = calculate_block_reward(current_height);
    let blockchain_size_bytes = blockchain.db.size_on_disk().unwrap_or(0);
    let next_halving_in_blocks = HALVING_INTERVAL - (current_height % HALVING_INTERVAL);
    let mempool_size = blockchain.mempool().len();

    // Improved hashrate calculation over the DAA window for more stability.
    let network_hashrate_hps = if current_height >= DAA_WINDOW as u64 {
        let window_start_index = current_height - (DAA_WINDOW as u64 - 1);
        if let (Ok(Some(start_block)), Ok(Some(end_block))) = (
            blockchain.get_block_by_index(window_start_index),
            blockchain.get_block_by_index(current_height),
        ) {
            let time_span_secs =
                (end_block.timestamp.timestamp() - start_block.timestamp.timestamp()).max(1);

            // Sum the work done over the window. Work is MAX_TARGET / target.
            let total_work: wisp_core::U256 = (window_start_index..=current_height)
                .filter_map(|i| {
                    if let Some((_, target)) = blockchain.daa_cache.get(&i) {
                        Some(*target)
                    } else {
                        blockchain
                            .get_block_by_index(i)
                            .ok()
                            .flatten()
                            .map(|b| b.target)
                    }
                })
                .map(|target| wisp_core::MAX_TARGET / target.max(wisp_core::MIN_TARGET))
                .fold(wisp_core::U256::zero(), |acc, work| acc + work);

            // Hashrate = (Total Hashes) / (Time). Total Hashes = Total Work * 2^32 (approx)
            // This is a more accurate estimation of hashrate.
            let total_hashes = u256_to_f64(total_work) * 2.0_f64.powi(32);
            total_hashes / time_span_secs as f64
        } else {
            0.0
        }
    } else {
        0.0
    };
    let confirmed_tx_count = blockchain
        .get_total_transaction_count_from_db()
        .unwrap_or(0);
    let total_transactions = confirmed_tx_count + mempool_size as u64;
    let vitals = ApiNetworkVitals {
        current_height,
        difficulty,
        avg_block_time_secs,
        reward_per_block_wisp: reward_per_block.to_string_wisp(),
        blockchain_size_bytes,
        next_halving_in_blocks,
        mempool_size,
        current_target,
        network_hashrate_hps,
        total_transactions,
    };

    Json(vitals).into_response()
}

/// Converts a U256 into an f64.
/// This is an approximation, as f64 has limited precision.
fn u256_to_f64(val: wisp_core::U256) -> f64 {
    let words = val.0;
    let mut result = 0.0;
    result += words[0] as f64;
    result += (words[1] as f64) * 2.0_f64.powi(64);
    result += (words[2] as f64) * 2.0_f64.powi(128);
    result += (words[3] as f64) * 2.0_f64.powi(192);
    result
}

mod serde_hash_str {
    use serde::{self, Serializer};
    use wisp_core::sha256::Hash;

    pub fn serialize<S>(val: &Hash, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&val.to_string())
    }
}

mod serde_pubkey_str {
    use serde::{self, Serializer};
    use wisp_core::signatures::PublicKey;

    pub fn serialize<S>(val: &PublicKey, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&val.fingerprint())
    }
}

mod serde_str {
    use serde::{self, Serializer};
    use wisp_core::U256;

    pub fn serialize<S>(val: &U256, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!("0x{:064x}", val))
    }
}

/// Helper function to process a block and enrich it with `time_to_mine_secs`.
fn process_and_enrich_block(
    blockchain: &wisp_core::blockchain::Blockchain,
    block: Block,
) -> axum::response::Response {
    let mut api_block: ApiBlock = block.clone().into();
    if api_block.height > 0 {
        if let Ok(Some(prev_block)) = blockchain.get_block_by_index(api_block.height - 1) {
            let time_diff = block.timestamp.timestamp() - prev_block.timestamp.timestamp();
            api_block.time_to_mine_secs = Some(time_diff);
        } else {
            log::warn!(
                "Could not find previous block for height {} to calculate mining time.",
                api_block.height
            );
        }
    }
    Json(api_block).into_response()
}

async fn get_block_by_height(
    State(blockchain_lock): State<Arc<RwLock<wisp_core::blockchain::Blockchain>>>,
    Path(height): Path<u64>,
) -> impl IntoResponse {
    let blockchain = blockchain_lock.read().await;
    match blockchain.get_block_by_index(height) {
        Ok(Some(block)) => process_and_enrich_block(&blockchain, block),
        Ok(None) => (StatusCode::NOT_FOUND, Json("Block not found".to_string())).into_response(),
        Err(e) => {
            log::error!("Failed to get block by height {}: {}", height, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error retrieving block from database",
            )
                .into_response()
        }
    }
}

async fn get_block_by_hash(
    State(blockchain_lock): State<Arc<RwLock<wisp_core::blockchain::Blockchain>>>,
    Path(hash_str): Path<String>,
) -> impl IntoResponse {
    let hash = match Hash::try_from(hash_str.as_str()) {
        Ok(h) => h,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, Json("Invalid block hash")).into_response();
        }
    };
    let blockchain = blockchain_lock.read().await;
    match blockchain.get_block_by_hash(&hash) {
        Ok(Some(block)) => process_and_enrich_block(&blockchain, block),
        Ok(None) => (StatusCode::NOT_FOUND, Json("Block not found".to_string())).into_response(),
        Err(e) => {
            log::error!("Failed to get block by hash {}: {}", hash, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error retrieving block from database",
            )
                .into_response()
        }
    }
}

async fn get_transaction_by_hash(
    State(blockchain_lock): State<Arc<RwLock<wisp_core::blockchain::Blockchain>>>,
    Path(hash_str): Path<String>,
) -> impl IntoResponse {
    let hash = match Hash::try_from(hash_str.as_str()) {
        Ok(h) => h,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, Json("Invalid transaction hash")).into_response();
        }
    };

    let blockchain = blockchain_lock.read().await;
    let tx_data = blockchain.get_transaction_with_details(&hash);
    match tx_data {
        Ok(Some((tx, block_height, timestamp))) => {
            let is_coinbase = tx.is_coinbase();
            let coinbase_message = if is_coinbase {
                tx.inputs
                    .first()
                    .and_then(|i| i.coinbase_data.as_ref())
                    .and_then(|data| {
                        // The first 8 bytes are the block height. The message is the rest.
                        // We must check if there are any bytes beyond the height.
                        let message_bytes =
                            data.get(std::mem::size_of::<u64>()..).unwrap_or_default();
                        String::from_utf8(message_bytes.to_vec()).ok()
                    })
            } else {
                None
            };
            let fee_or_reward = if is_coinbase {
                tx.outputs.iter().map(|o| o.value).sum()
            } else {
                blockchain
                    .calculate_transaction_fee(&tx)
                    .unwrap_or_else(|_| Amount::zero())
            };

            let total_output: Amount = tx.outputs.iter().map(|o| o.value).sum();

            let inputs = if is_coinbase {
                // For coinbase, the input is special and contains the message.
                vec![ApiTransactionInput {
                    outpoint: "Coinbase (New Coins)".to_string(),
                    signature: None,
                }]
            } else {
                tx.inputs
                    // For regular transactions, map the inputs normally.
                    .iter()
                    .map(|i| ApiTransactionInput {
                        outpoint: i.outpoint.to_string(),
                        signature: i.signature.as_ref().map(|s| hex::encode(s.0.to_bytes())),
                    })
                    .collect()
            };

            let api_tx = ApiTransactionDetail {
                hash: tx.txid().unwrap_or_default(),
                block_height,
                timestamp: timestamp.timestamp(),
                fee_or_reward: fee_or_reward.to_string_wisp(),
                total_output_wisp: total_output.to_string_wisp(),
                inputs,
                outputs: tx
                    .outputs
                    .iter()
                    .map(|o| ApiTransactionOutput {
                        value: o.value.to_string_wisp(),
                        pubkey: o.pubkey.clone(),
                    })
                    .collect(),
                coinbase_message,
            };

            Json(api_tx).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, Json("Transaction not found")).into_response(),
        Err(e) => {
            log::error!("Error fetching transaction by hash {}: {}", hash, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error retrieving transaction",
            )
                .into_response()
        }
    }
}

async fn get_blocks_paginated(
    State(blockchain_lock): State<Arc<RwLock<wisp_core::blockchain::Blockchain>>>,
    Query(pagination): Query<Pagination>,
) -> impl IntoResponse {
    let page = pagination.page.unwrap_or(1);
    let limit = pagination.limit.unwrap_or(25);

    if page == 0 || limit == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json("Page and limit must be greater than 0".to_string()),
        )
            .into_response();
    }

    let blockchain = blockchain_lock.read().await;
    let total_blocks = match blockchain.block_height() {
        Ok(height) => height + 1,
        Err(e) => {
            log::error!("Failed to get chain height for pagination: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to get chain height",
            )
                .into_response();
        }
    };

    let mut blocks: Vec<ApiBlock> = Vec::with_capacity(limit as usize);
    if total_blocks > 0 {
        let highest_index = total_blocks - 1;
        let start_index = highest_index.saturating_sub((page - 1) * limit);
        let end_index = start_index.saturating_sub(limit.saturating_sub(1));
        for i in (end_index..=start_index).rev() {
            match blockchain.get_block_by_index(i) {
                Ok(Some(block)) => {
                    let mut api_block: ApiBlock = block.clone().into();
                    if i > 0 {
                        if let Ok(Some(prev_block)) = blockchain.get_block_by_index(i - 1) {
                            api_block.time_to_mine_secs = Some(
                                block.timestamp.timestamp() - prev_block.timestamp.timestamp(),
                            );
                        }
                    }
                    blocks.push(api_block);
                }
                Ok(None) => {}
                Err(e) => {
                    log::error!("Error fetching block {} for paginated response: {}", i, e);
                }
            }
        }
    }

    Json(PaginatedBlocksResponse {
        data: blocks,
        page,
        limit,
        total_blocks,
    })
    .into_response()
}

async fn get_recent_transactions(
    State(blockchain_lock): State<Arc<RwLock<wisp_core::blockchain::Blockchain>>>,
) -> impl IntoResponse {
    let blockchain = blockchain_lock.read().await;
    let max_recent_txs = 100;
    let mempool = blockchain.mempool();
    let mut api_transactions: Vec<ApiTransactionSummary> = mempool
        .iter()
        .map(|(_tx_hash, entry)| {
            let total_output: wisp_core::currency::Amount = entry
                .transaction
                .outputs
                .iter()
                .map(|o| o.value)
                .sum();
            if total_output == Amount::MAX {
                log::warn!("Overflow calculating total output for a recent transaction. This should not happen for a validated tx.");
            }

            ApiTransactionSummary {
                hash: entry.transaction.txid().unwrap_or_default(),
                block_height: None,
                input_count: entry.transaction.inputs.len(),
                is_coinbase: false,
                output_count: entry.transaction.outputs.len(),
                total_output_wisp: total_output.to_string_wisp(),
                timestamp: entry.timestamp.timestamp(),
            }
        })
        .collect();

    let current_height = blockchain.block_height().unwrap_or(0);
    let start_block = if current_height > 4 {
        current_height - 4
    } else {
        0
    };

    for i in (start_block..=current_height).rev() {
        if api_transactions.len() >= max_recent_txs {
            break;
        }

        if let Ok(Some(block)) = blockchain.get_block_by_index(i) {
            for tx in block.transactions.iter() {
                let total_output: wisp_core::currency::Amount =
                    tx.outputs.iter().map(|o| o.value).sum();
                if total_output == Amount::MAX {
                    log::warn!("Overflow calculating total output for a block transaction. This should not happen for a validated tx.");
                }

                api_transactions.push(ApiTransactionSummary {
                    hash: tx.txid().unwrap_or_default(),
                    block_height: Some(block.index),
                    input_count: tx.inputs.len(),
                    is_coinbase: tx.is_coinbase(),
                    output_count: tx.outputs.len(),
                    total_output_wisp: total_output.to_string_wisp(),
                    timestamp: block.timestamp.timestamp(),
                });

                if api_transactions.len() >= max_recent_txs {
                    break;
                }
            }
        }
    }

    api_transactions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    api_transactions.truncate(max_recent_txs);
    let info = MempoolInfo {
        transactions: api_transactions.clone(),
        count: api_transactions.len(),
    };

    Json(info).into_response()
}

async fn get_transactions_paginated(
    State(blockchain_lock): State<Arc<RwLock<wisp_core::blockchain::Blockchain>>>,
    Query(pagination): Query<Pagination>,
) -> impl IntoResponse {
    let page = pagination.page.unwrap_or(1);
    let limit = pagination.limit.unwrap_or(25);

    if page == 0 || limit == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json("Page and limit must be greater than 0".to_string()),
        )
            .into_response();
    }

    let blockchain = blockchain_lock.read().await;
    let mempool = blockchain.mempool().clone();
    let confirmed_tx_count = blockchain
        .get_total_transaction_count_from_db()
        .unwrap_or(0);
    let total_transactions = confirmed_tx_count + mempool.len() as u64;
    let page_start_index = (page - 1) * limit;
    let mut transactions_for_page: Vec<ApiTransactionSummary> = Vec::with_capacity(limit as usize);
    let mut mempool_txs: Vec<ApiTransactionSummary> = mempool
        .iter()
        .map(|(_tx_hash, entry)| {
            let total_output: wisp_core::currency::Amount =
                entry.transaction.outputs.iter().map(|o| o.value).sum();
            ApiTransactionSummary {
                hash: entry.transaction.txid().unwrap_or_default(),
                block_height: None,
                input_count: entry.transaction.inputs.len(),
                is_coinbase: false,
                output_count: entry.transaction.outputs.len(),
                total_output_wisp: total_output.to_string_wisp(),
                timestamp: entry.timestamp.timestamp(),
            }
        })
        .collect();
    mempool_txs.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    let mempool_count = mempool_txs.len() as u64;
    if page_start_index < mempool_count {
        let mempool_to_add = mempool_txs
            .iter()
            .skip(page_start_index as usize)
            .take(limit as usize)
            .cloned();
        transactions_for_page.extend(mempool_to_add);
    }

    let needed_from_db = limit.saturating_sub(transactions_for_page.len() as u64);
    if needed_from_db > 0 {
        let db_tx_to_skip = page_start_index.saturating_sub(mempool_count);
        if confirmed_tx_count > 0 {
            let highest_confirmed_idx = confirmed_tx_count - 1;
            let start_idx = highest_confirmed_idx.saturating_sub(db_tx_to_skip);
            let end_idx = start_idx.saturating_sub(needed_from_db - 1);
            for i in (end_idx..=start_idx).rev() {
                if let Ok(Some(tx_hash)) = blockchain.get_transaction_hash_by_chronological_index(i)
                {
                    if let Ok(Some((tx, block_height, timestamp))) =
                        blockchain.get_transaction_with_details(&tx_hash)
                    {
                        let total_output: wisp_core::currency::Amount =
                            tx.outputs.iter().map(|o| o.value).sum();

                        transactions_for_page.push(ApiTransactionSummary {
                            hash: tx_hash,
                            block_height,
                            input_count: tx.inputs.len(),
                            is_coinbase: tx.is_coinbase(),
                            output_count: tx.outputs.len(),
                            total_output_wisp: total_output.to_string_wisp(),
                            timestamp: timestamp.timestamp(),
                        });
                    }
                }
            }
        }
    }

    Json(PaginatedTransactionsResponse {
        data: transactions_for_page,
        page,
        limit,
        total_transactions,
    })
    .into_response()
}
