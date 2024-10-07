mod blockchain;
use blockchain::{Blockchain, Transaction, generate_keypair};
use ring::signature::KeyPair;
use hex;

fn main() {
    // 1. Generate key pairs for participants
    let miner_keypair = generate_keypair();
    let user1_keypair = generate_keypair();
    let user2_keypair = generate_keypair();

    // 2. Create a new blockchain
    let mut blockchain = Blockchain::new();

    // 3. Create and add more than 100 transactions to the mempool
    for i in 0..150 {
        let sender_keypair = if i % 2 == 0 { &user1_keypair } else { &user2_keypair };
        let receiver_public_key = if i % 2 == 0 { user2_keypair.public_key().as_ref().to_vec() } else { user1_keypair.public_key().as_ref().to_vec() };

        // Create and sign the transaction
        let mut transaction = Transaction::new(sender_keypair.public_key().as_ref().to_vec(), receiver_public_key, i as u64 + 1);
        transaction.sign(sender_keypair);

        // Add the transaction to the mempool
        blockchain.add_transaction(transaction);
    }

    // 4. Mine all blocks until mempool is empty
    blockchain.add_block("Block 1".to_string(), &miner_keypair);

    // 5. Print the entire blockchain
    println!("\n🔗 Blockchain:");
    for block in blockchain.get_chain() {
        println!("Block {}: ", block.index);
        println!("  Hash: {}", block.hash);
        println!("  Previous Hash: {}", block.previous_hash);
        println!("  Nonce: {}", block.nonce);
        println!("  Timestamp: {}", block.timestamp);
        println!("  Transactions: ");
        for tx in &block.transactions {
            println!("    Sender: {}", hex::encode(&tx.sender));
            println!("    Receiver: {}", hex::encode(&tx.receiver));
            println!("    Amount: {}", tx.amount);
            println!("    Signature: {}", hex::encode(&tx.signature));
        }
        println!("--------------------------------------");
    }
}