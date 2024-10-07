use ring::digest::{Context, SHA256};     
use ring::signature::{Ed25519KeyPair, KeyPair, Signature};            
use ring::signature;                     
use ring::rand::SystemRandom;            
use serde::{Serialize, Deserialize};     
use chrono::Utc;                         
use rayon::prelude::*;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Transaction {
    pub sender: Vec<u8>,
    pub receiver: Vec<u8>,
    pub amount: u64,
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

impl Transaction {
    pub fn new(sender: Vec<u8>, receiver: Vec<u8>, amount: u64) -> Transaction {
        Transaction {
            sender,
            receiver,
            amount,
            signature: vec![],
        }
    }

    pub fn sign(&mut self, keypair: &Ed25519KeyPair) {
        let message = self.get_message_for_signing();
        let signature: Signature = keypair.sign(&message);
        self.signature = signature.as_ref().to_vec();
    }

    pub fn verify_signature(&self, public_key: &[u8]) -> bool {
        let public_key = signature::UnparsedPublicKey::new(&signature::ED25519, public_key);
        let message = self.get_message_for_signing();
        public_key.verify(&message, &self.signature).is_ok()
    }

    fn get_message_for_signing(&self) -> Vec<u8> {
        let mut data = vec![];
        data.extend_from_slice(&self.sender);
        data.extend_from_slice(&self.receiver);
        data.extend_from_slice(&self.amount.to_le_bytes());
        data
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Block {
    pub index: u64,
    pub data: String,
    pub nonce: u64,
    pub transactions: Vec<Transaction>,
    pub timestamp: u128,
    pub previous_hash: String,
    pub hash: String,
}

impl Block {
    fn new(index: u64, data: String, nonce: u64, transactions: Vec<Transaction>, timestamp: u128, previous_hash: String) -> Self {
        let mut block = Block {
            index,
            data,
            nonce,
            transactions,
            timestamp,
            previous_hash,
            hash: String::new(),
        };

        block.hash = Block::calculate_hash(&block);
        block
    }

    fn calculate_hash(block: &Block) -> String {
        let serialized_block = serde_json::to_string(block).unwrap();
        let mut context = Context::new(&SHA256);
        context.update(serialized_block.as_bytes());
        let hash_result = context.finish();
        hex::encode(hash_result)
    }

    pub fn genesis() -> Block {
        const GENESIS_TIMESTAMP: u128 = 1_690_000_000_000;
        Block::new(0, String::from("Genesis Block"), 0, Vec::new(), GENESIS_TIMESTAMP, String::from("0"))
    }
}

pub struct Blockchain {
    chain: Vec<Block>,
    pub mempool: Vec<Transaction>,
    difficulty: usize,
    miner_address: Vec<u8>,
    reward: u64,
}

impl Blockchain {
    pub fn new() -> Self {
        let mut blockchain = Blockchain { 
            chain: Vec::new(),
            mempool: Vec::new(),
            difficulty: 6,
            miner_address: vec![1, 2, 3, 4, 5],
            reward: 5,
        };
        blockchain.chain.push(Block::genesis());
        blockchain
    }

    pub fn add_transaction(&mut self, tx: Transaction) {
        if tx.verify_signature(&tx.sender) {
            self.mempool.push(tx);
        } else {
            println!("⚠️ Invalid transaction signature from sender: {}", hex::encode(&tx.sender));
        }
    }

    fn latest_block(&self) -> &Block {
        self.chain.last().unwrap()
    }

    pub fn mine_block(&self, last_nonce: u64, difficulty: usize) -> u64 {
        (0..u64::MAX)
            .into_par_iter()
            .find_any(|&nonce| Blockchain::valid_proof(last_nonce, nonce, difficulty))
            .expect("No valid nonce found!")
    }

    fn valid_proof(last_nonce: u64, nonce: u64, difficulty: usize) -> bool {
        let guess = format!("{}{}", last_nonce, nonce);
        let mut context = Context::new(&SHA256);
        context.update(guess.as_bytes());
        let digest = context.finish();
        let guess_hash = hex::encode(digest);
        guess_hash.starts_with(&"0".repeat(difficulty))
    }

    pub fn add_block(&mut self, data: String, keypair: &Ed25519KeyPair) {
        println!("🔨 Mining a new block...");
        while !self.mempool.is_empty() {
            let limited_transactions = self.mempool.drain(..100.min(self.mempool.len())).collect::<Vec<_>>();
            let previous_block = self.latest_block();
            let previous_index = previous_block.index;
            let previous_hash = previous_block.hash.clone();
            let previous_nonce = previous_block.nonce;
            let timestamp = Utc::now().timestamp_millis() as u128;
            let miner_public_key = keypair.public_key().as_ref().to_vec();
            let mut reward_transaction = Transaction::new(
                miner_public_key.clone(),
                miner_public_key.clone(),
                self.reward,
            );
            reward_transaction.sign(keypair);
            let mut all_transactions = limited_transactions;
            all_transactions.push(reward_transaction);
            let nonce = self.mine_block(previous_nonce, self.difficulty);
            let new_block = Block::new(
                previous_index + 1,
                data.clone(),
                nonce,
                all_transactions,
                timestamp,
                previous_hash.clone(),
            );
            self.chain.push(new_block);
            let block_number = previous_index + 1;
            let block_hash = &self.chain.last().unwrap().hash;
            let total_blocks = self.chain.len();
            println!("\n🌟 Block Mined Successfully! 🌟");
            println!("--------------------------------------");
            println!("🔢 Block Number: {}", block_number);
            println!("⏰ Timestamp: {}", Utc::now().to_rfc3339());
            println!("🔗 Previous Block Hash: {}", previous_hash);
            println!("🔨 Nonce: {}", nonce);
            println!("💰 Reward Transaction: {} coins to {}", self.reward, hex::encode(&self.miner_address));
            println!("💎 Current Block Hash: {}", block_hash);
            println!("📦 Total Blocks in Chain: {}", total_blocks);
            println!("--------------------------------------");
            println!("✅ Block {} mined and added to the blockchain successfully! 🎉\n", block_number);
        }
    }     

    pub fn get_chain(&self) -> &Vec<Block> {
        &self.chain
    }
}

pub fn generate_keypair() -> signature::Ed25519KeyPair {
    let rng = SystemRandom::new();
    let pkcs8_bytes = signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    signature::Ed25519KeyPair::from_pkcs8(pkcs8_bytes.as_ref()).unwrap()
}