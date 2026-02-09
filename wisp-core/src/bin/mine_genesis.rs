use anyhow::Result;
use chrono::Utc;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
};
use wisp_core::{
    blockchain::Block,
    currency::Amount,
    pow::mine_block_parallel,
    sha256::Hash,
    signatures::PublicKey,
    transactions::{OutPoint, Transaction, TransactionInput, TransactionOutput},
    utils::MerkleRoot,
    INITIAL_BLOCK_REWARD_SMALLEST_UNITS, MAX_TARGET,
};

/// A utility to find a valid nonce for the genesis block.
fn main() -> Result<()> {
    // --- Step 1: CONFIGURE YOUR GENESIS BLOCK PARAMETERS HERE ---
    let genesis_message = "Sic Mundus Creatus Est // 5.11.2025 //";
    let genesis_timestamp = Utc::now();
    // ---

    // --- Step 2: The tool will now construct and mine the block ---
    println!("Configuration:");
    println!("  Message:   '{}'", genesis_message);
    println!("  Timestamp: '{}' (current time)", genesis_timestamp);
    println!("---------------------------------------------------");

    let genesis_pubkey_hex = "020000000000000000000000000000000000000000000000000000000000000001";
    let genesis_pubkey_bytes = hex::decode(genesis_pubkey_hex)?;
    let genesis_verifying_key = k256::ecdsa::VerifyingKey::from_sec1_bytes(&genesis_pubkey_bytes)?;

    let mut coinbase_data = Vec::new();
    coinbase_data.extend_from_slice(&0u64.to_le_bytes()); // Block height 0
    coinbase_data.extend_from_slice(genesis_message.as_bytes());
    let coinbase_tx = Transaction::new(
        vec![TransactionInput {
            outpoint: OutPoint {
                txid: Hash::zero(),
                vout: u32::MAX,
            },
            signature: None,
            coinbase_data: Some(coinbase_data),
        }],
        vec![TransactionOutput {
            value: Amount::from_smallest_unit(INITIAL_BLOCK_REWARD_SMALLEST_UNITS),
            pubkey: PublicKey(genesis_verifying_key),
        }],
    );

    let merkle_root = MerkleRoot::calculate(&[coinbase_tx.clone()])?;

    let genesis_block = Block::new(
        1,
        genesis_timestamp,
        0,
        Hash::zero(),
        merkle_root,
        MAX_TARGET,
        0,
        vec![coinbase_tx],
    );

    let num_threads = thread::available_parallelism()?.get();
    println!(
        "Mining genesis block using {} threads... (This may take a while)",
        num_threads
    );
    println!("Target: {}", MAX_TARGET);

    let mining_active = Arc::new(AtomicBool::new(true));
    let found_block = Arc::new(Mutex::new(None));
    let mut handles = vec![];

    for i in 0..num_threads {
        let mining_active_clone = Arc::clone(&mining_active);
        let found_block_clone = Arc::clone(&found_block);
        let genesis_block_clone = genesis_block.clone();

        let handle = thread::spawn(move || {
            let start_nonce = i as u64;
            let nonce_step = num_threads as u64;
            let mut block_clone = genesis_block_clone;

            while mining_active_clone.load(Ordering::Relaxed) {
                if let Ok((found, _attempts)) = mine_block_parallel(
                    &mut block_clone,
                    start_nonce,
                    nonce_step,
                    1_000_000,
                    &mining_active_clone,
                ) {
                    if found {
                        let mut found = found_block_clone.lock().unwrap();
                        if found.is_none() {
                            *found = Some(block_clone.clone());
                            mining_active_clone.store(false, Ordering::Relaxed);
                        }
                    }
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    if let Some(block) = found_block.lock().unwrap().clone() {
        println!("\n🎉 Found valid genesis block! 🎉");
        println!("Nonce:      {}", block.header.nonce);
        println!("Timestamp:  {}", block.header.timestamp);
        println!("Hash:       {}", block.id()?.to_string());
        println!("\nACTION: Copy the 'Nonce' and 'Timestamp' values into the `genesis_block` function in `wisp-core/src/utils.rs`.");
    }
    Ok(())
}
