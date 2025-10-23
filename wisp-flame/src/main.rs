use anyhow::{anyhow, Result};
use chrono::{Duration as ChronoDuration, Utc};
use clap::{arg, command, Parser};
use k256::ecdsa::VerifyingKey;
use log::{debug, error, info, trace, warn};
use num_cpus;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::net::TcpStream;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::interval;
use wisp_core::network::Message;
use wisp_core::{
    blockchain::Block, pow::mine_block_parallel, signatures::PublicKey, MAX_BLOCK_FUTURE_TIMESTAMP,
};

#[derive(Parser, Debug)]
/// Defines the command-line arguments for the Flame miner.
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    node_address: String,

    #[arg(short, long)]
    reward_address: String,
}

struct Miner {
    public_key: PublicKey,
    stream: AsyncMutex<TcpStream>,
    current_template: Arc<Mutex<Option<Block>>>,
    mining: Arc<AtomicBool>,
    new_template_counter: Arc<AtomicU64>,
    mined_block_sender: flume::Sender<Block>,
    mined_block_receiver: flume::Receiver<Block>,
    total_hashes: Arc<AtomicU64>,
}

impl Miner {
    async fn new(address: String, public_key: PublicKey) -> Result<Self> {
        info!("Connecting to node at {}", address);
        let stream = AsyncMutex::new(
            TcpStream::connect(&address)
                .await
                .map_err(|e| anyhow!("Failed to connect to node {}: {}", address, e))?,
        );
        info!("Successfully connected to node at {}", address);
        let (mined_block_sender, mined_block_receiver) = flume::unbounded();
        Ok(Self {
            public_key,
            stream,
            current_template: Arc::new(Mutex::new(None)),
            mining: Arc::new(AtomicBool::new(false)),
            new_template_counter: Arc::new(AtomicU64::new(0)),
            mined_block_sender,
            mined_block_receiver,
            total_hashes: Arc::new(AtomicU64::new(0)),
        })
    }

    async fn run(&self) -> Result<()> {
        let _mining_handles = self.spawn_mining_thread();
        let mut hashrate_report_interval = interval(Duration::from_secs(30));
        let mut ping_interval = interval(Duration::from_secs(60)); // Keep-alive interval

        // Spawn a separate task to report the hashrate periodically.
        let total_hashes_clone = self.total_hashes.clone();
        tokio::spawn(async move {
            let mut last_report_time = Instant::now();
            loop {
                hashrate_report_interval.tick().await;
                let elapsed = last_report_time.elapsed().as_secs_f64();
                let hashes = total_hashes_clone.swap(0, Ordering::Relaxed);
                if elapsed > 0.0 {
                    let hashrate = hashes as f64 / elapsed;
                    info!("Total Hashrate: {:.2} kH/s", hashrate / 1000.0);
                }
                last_report_time = Instant::now();
            }
        });

        // Fetch the initial block template to start mining.
        self.fetch_and_validate_template().await?;

        loop {
            let receiver_clone = self.mined_block_receiver.clone();

            tokio::select! {
                // Branch 1: A mining thread has found a block.
                Ok(mined_block) = receiver_clone.recv_async() => {
                    info!("Received mined block from mining thread.");
                    if self.submit_block(mined_block).await.is_ok() { // The block was accepted
                        info!("Block accepted by node. Requesting new template...");
                        self.fetch_and_validate_template().await?;
                    } else {
                        warn!("Block was rejected. Fetching a new template to resume mining.");
                        self.fetch_and_validate_template().await?;
                    }
                },
                // Branch 2: Listen for messages from the node, like a new template.
                msg_res = async { let mut stream = self.stream.lock().await; Message::receive_async(&mut *stream).await } => {
                    match msg_res {
                        Ok(Message::NewTemplate(template)) => {
                            info!("Received new template from node for block #{}. Updating...", template.index);
                            let mut current_template_guard = self.current_template.lock().unwrap();
                            *current_template_guard = Some(template.clone());
                            self.new_template_counter.fetch_add(1, Ordering::Relaxed);
                            self.mining.store(true, Ordering::Relaxed); // Ensure mining is active
                        },
                        Ok(other) => trace!("Received other message from node: {:?}", other),
                        Err(e) => {
                            warn!("Error reading message from node: {}. Pausing mining and attempting recovery.", e);
                            self.mining.store(false, Ordering::Relaxed);
                            if let Err(e) = self.fetch_and_validate_template().await {
                                warn!("Recovery attempt failed: {}. Miner will retry later.", e);
                            }
                        }
                    }
                },
                // Branch 3: Periodically send a Ping to keep the connection alive.
                _ = ping_interval.tick() => {
                    debug!("Sending Ping to node to keep connection alive.");
                    let mut stream_lock = self.stream.lock().await;
                    if let Err(e) = Message::Ping.send_async(&mut *stream_lock).await {
                        warn!("Failed to send Ping to node: {}. Connection may be lost.", e);
                    }
                }
            }
        }
    }

    fn spawn_mining_thread(&self) -> Vec<thread::JoinHandle<()>> {
        let num_threads = num_cpus::get();
        let mut handles = Vec::with_capacity(num_threads);
        info!("Spawning {} mining threads...", num_threads);

        for i in 0..num_threads {
            let current_template = self.current_template.clone();
            let mining_active = self.mining.clone();
            let mined_block_sender = self.mined_block_sender.clone();
            let new_template_counter = self.new_template_counter.clone();
            let total_hashes = self.total_hashes.clone();

            let handle = thread::spawn(move || {
                let mut last_template_id = 0;

                loop {
                    if !mining_active.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(500));
                        continue;
                    }

                    let template_block = match current_template.lock().unwrap().clone() {
                        Some(block) => block,
                        None => {
                            thread::sleep(Duration::from_millis(500));
                            continue;
                        }
                    };

                    let current_template_id = new_template_counter.load(Ordering::Relaxed);
                    if last_template_id != current_template_id {
                        debug!("Thread {} detected new template.", i);
                        last_template_id = current_template_id;
                    }

                    let mut block_to_mine = template_block;
                    // CRITICAL: Update the timestamp before each mining cycle.
                    block_to_mine.timestamp = Utc::now();

                    let nonce_step = num_threads as u64;
                    let start_nonce = i as u64;
                    let max_attempts_per_call = 1_000_000;

                    match mine_block_parallel(
                        &mut block_to_mine,
                        start_nonce,
                        nonce_step,
                        max_attempts_per_call,
                        &mining_active,
                    ) {
                        Ok((found, attempts)) => {
                            total_hashes.fetch_add(attempts as u64, Ordering::Relaxed);
                            if found {
                                // The block is now mined, so its hash is final.
                                if let Ok(block_hash) = block_to_mine.id() {
                                    info!(
                                        "\n⛏️  Block Found! (Thread {})\n   - Index: {}\n   - Nonce: {}\n   - Hash: {}",
                                        i, block_to_mine.index, block_to_mine.nonce, block_hash
                                    );
                                }
                                // Stop all other threads from mining and send the block for submission.
                                if mined_block_sender.send(block_to_mine).is_ok() {
                                    mining_active.store(false, Ordering::Relaxed);
                                }
                            }
                        }
                        Err(e) => {
                            error!("Mining thread {} encountered an error: {}", i, e);
                        }
                    }
                }
            });
            handles.push(handle);
        }
        handles
    }

    async fn fetch_and_validate_template(&self) -> Result<()> {
        self.mining.store(false, Ordering::Relaxed);
        info!("Requesting new block template from node...");
        let message = Message::FetchTemplate(self.public_key.clone());
        let mut stream_lock = self.stream.lock().await;
        message.send_async(&mut *stream_lock).await?;

        match tokio::time::timeout(
            Duration::from_secs(5),
            Message::receive_async(&mut *stream_lock),
        )
        .await
        {
            Ok(Ok(Message::Template(mut template))) => {
                let now = Utc::now();
                if template.timestamp
                    > now + ChronoDuration::seconds(MAX_BLOCK_FUTURE_TIMESTAMP as i64)
                {
                    warn!("Received template with timestamp too far in the future. Adjusting to current time.");
                    template.timestamp = now;
                }

                info!(
                    "Received new template for block #{}. Resuming mining.",
                    template.index
                );
                let mut current_template_guard = self.current_template.lock().unwrap();
                *current_template_guard = Some(template);
                self.new_template_counter.fetch_add(1, Ordering::Relaxed);
                self.mining.store(true, Ordering::Relaxed);
                Ok(())
            }
            Ok(Ok(other)) => Err(anyhow!(
                "Unexpected message received while fetching template: {:?}",
                other
            )),
            Ok(Err(e)) => Err(anyhow!("Error receiving template from node: {}", e)),
            Err(_) => Err(anyhow!("Timeout waiting for template from node")),
        }
    }

    /// Submits a successfully mined block to the node.
    async fn submit_block(&self, block: Block) -> Result<()> {
        info!(
            "Submitting mined block: {}",
            block.id().expect("Failed to hash mined block for logging")
        );
        let message = Message::SubmitTemplate(block);

        let mut stream_lock = self.stream.lock().await;
        message
            .send_async(&mut *stream_lock)
            .await
            .map_err(|e| anyhow!("Failed to send SubmitTemplate message: {}", e))?;

        info!("Mined block submitted, waiting for confirmation...");

        // Wait for the node's response.
        match tokio::time::timeout(
            Duration::from_secs(5),
            Message::receive_async(&mut *stream_lock),
        )
        .await
        {
            Ok(Ok(Message::BlockSubmittedConfirmation)) => {
                info!("✅ Submission successful! Block accepted by node.");
                Ok(())
            }
            Ok(Ok(Message::BlockRejected(reason))) => {
                warn!("Block submission rejected by node: {}", reason);
                Err(anyhow!("Block submission rejected by node: {}", reason))
            }
            Ok(Ok(other)) => {
                warn!(
                    "Unexpected message received after submitting block: {:?}",
                    other
                );
                Err(anyhow!(
                    "Unexpected message received after submitting block: {:?}",
                    other
                ))
            }
            Ok(Err(e)) => {
                error!("Failed to receive confirmation response from node: {}", e);
                Err(anyhow!("Failed to receive confirmation response: {}", e))
            }
            Err(_) => {
                error!("Timeout waiting for block submission confirmation from node.");
                Err(anyhow!("Timeout waiting for block submission confirmation"))
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Use try_init to be robust in a workspace environment.
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("wisp_flame=info"),
    )
    .try_init();

    let args = Args::parse();

    // Parse the reward address (public key) from the command line.
    let public_key_bytes = hex::decode(&args.reward_address)
        .map_err(|e| anyhow!("Invalid reward public key format (must be hex): {}", e))?;
    let verifying_key = VerifyingKey::from_sec1_bytes(&public_key_bytes)
        .map_err(|e| anyhow!("Invalid public key bytes: {}", e))?;
    let public_key = PublicKey(verifying_key);

    info!(
        "\n\n\
        🔥 Starting Wisp Flame Miner\n\
        --------------------------------------------------\n\
        - Node Address:   {}\n\
        - Reward Address: {}\n\
        - CPU Threads:    {}\n\
        --------------------------------------------------\n",
        args.node_address,
        public_key.fingerprint(),
        num_cpus::get()
    );

    let miner = Miner::new(args.node_address, public_key).await?;
    miner.run().await?;

    Ok(())
}
