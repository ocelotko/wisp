use anyhow::{anyhow, Result};
use clap::Parser;
use hex;
use k256::ecdsa::VerifyingKey;
use log::{debug, error, info, trace, warn};
use num_cpus;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::{thread, time::Instant};
use tokio::net::TcpStream;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::{interval, Duration};
use wisp_core::{
    blockchain::Block, network::Message, pow::mine_block_parallel, signatures::PublicKey,
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

/// The main struct representing the state and logic of the miner.
struct Miner {
    public_key: PublicKey,
    stream: AsyncMutex<TcpStream>,
    current_template: Arc<AsyncMutex<Option<Block>>>,
    mining: Arc<AtomicBool>,
    mined_block_sender: flume::Sender<Block>,
    mined_block_receiver: flume::Receiver<Block>,
    total_hashes: Arc<std::sync::atomic::AtomicU64>,
}

impl Miner {
    /// Creates a new `Miner` instance and connects to the specified node.
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
            current_template: Arc::new(AsyncMutex::new(None)),
            mining: Arc::new(AtomicBool::new(false)),
            mined_block_sender,
            mined_block_receiver,
            total_hashes: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        })
    }

    /// The main run loop for the miner.
    /// This function orchestrates fetching templates, mining, and submitting blocks.
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
            let mut stream_lock = self.stream.lock().await;

            tokio::select! {
                // A mining thread has found a block.
                Ok(mined_block) = receiver_clone.recv_async() => {
                    drop(stream_lock); // Release lock before submitting
                    info!("Received mined block from mining thread.");
                    if self.submit_block(mined_block).await.is_ok() {
                        // After successful submission, immediately fetch a new template
                        // to resume mining without delay.
                        info!("Block accepted by node. Requesting new template and resuming mining...");
                        self.fetch_and_validate_template().await?;
                    } else {
                        // If the block was rejected, it might be because the template was stale.
                        // Fetch a new one to be sure.
                        warn!("Block was rejected. Fetching a new template.");
                        self.fetch_and_validate_template().await?;
                    }
                },
                // Listen for messages from the node, like a new template.
                msg_res = Message::receive_async(&mut *stream_lock) => {
                    match msg_res {
                        Ok(message) => {
                            match message {
                                Message::NewTemplate(template) => {
                                    info!("Received new template from node for block #{}. Updating...", template.index);
                                    let mut current_template_guard = self.current_template.lock().await;
                                    *current_template_guard = Some(template);
                                    self.mining.store(true, Ordering::Relaxed); // Ensure mining is active
                                },
                                other => {
                                    trace!("Received other message from node: {:?}", other);
                                }
                            }
                        }
                        // If reading from the node failed, pause mining and try to recover.
                        Err(e) => {
                            warn!("Error reading message from node: {}. Pausing mining and attempting recovery.", e);
                            self.mining.store(false, Ordering::Relaxed);
                            // Drop lock before attempting recovery.
                            drop(stream_lock);
                            // Try to re-fetch template (this will attempt to use the stream; if it's broken,
                            // fetch_and_validate_template should gracefully handle send/receive errors).
                            if let Err(e) = self.fetch_and_validate_template().await {
                                warn!("Recovery attempt failed: {}. Miner will retry later.", e);
                                // Let loop continue; ping/reconnect logic should be added separately if needed.
                            }
                        }
                    }
                },
                // Periodically send a Ping to keep the connection alive.
                _ = ping_interval.tick() => {
                    debug!("Sending Ping to node to keep connection alive.");
                    if let Err(e) = Message::Ping.send_async(&mut *stream_lock).await {
                        warn!("Failed to send Ping to node: {}. Connection may be lost.", e);
                    }
                }
            }
        }
    }

    /// Spawns a mining thread for each available CPU core.
    /// Each thread works on a different part of the nonce range.
    fn spawn_mining_thread(&self) -> Vec<thread::JoinHandle<()>> {
        let num_cores = num_cpus::get().max(1); // Ensure at least one thread
        info!("Spawning {} mining threads.", num_cores);
        let mut handles = Vec::new();
        for i in 0..num_cores {
            let template_clone = self.current_template.clone();
            let mining_clone = self.mining.clone();
            let sender_clone = self.mined_block_sender.clone();
            let total_hashes_clone = self.total_hashes.clone();
            let thread_id = i;
            let handle = thread::spawn(move || {
                info!("Mining thread {} started.", thread_id);

                loop {
                    // If mining is paused (e.g., after finding a block), sleep and continue.
                    if !mining_clone.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }

                    // Get a local copy of the current template to work on.
                    let mut current_block = match template_clone.blocking_lock().clone() {
                        Some(template) => template,
                        None => {
                            // This can happen on startup before the first template is fetched.
                            std::thread::sleep(Duration::from_millis(500));
                            continue;
                        }
                    };

                    trace!(
                        "Thread {} mining block with target: {:?}",
                        thread_id,
                        current_block.target
                    );

                    let mining_start_time = std::time::Instant::now();
                    let max_attempts_per_call = 1_000_000;

                    // Call the parallel mining function which iterates through nonces.
                    match mine_block_parallel(
                        &mut current_block,
                        thread_id as u64, // Each thread gets a unique starting nonce offset
                        num_cores as u64, // Threads increment by the number of cores to avoid overlap
                        max_attempts_per_call,
                        &mining_clone,
                    ) {
                        // A valid block was found!
                        Ok((true, attempts)) => {
                            if let Ok(block_hash) = current_block.id() {
                                info!(
                                    "\n✅ Block Found by Thread {}!\n   - Index: {}\n   - Nonce: {}\n   - Hash:  {}\n   - Time:  {:?}",
                                    thread_id,
                                    current_block.index,
                                    current_block.nonce,
                                    block_hash,
                                    mining_start_time.elapsed()
                                );
                            } else {
                                info!(
                                    "✅ Block Found by Thread {} after {attempts} attempts!",
                                    thread_id
                                );
                                warn!(
                                    "Failed to calculate hash for mined block by thread {}.",
                                    thread_id
                                );
                            }

                            // Atomically check if mining is still active before sending.
                            // If it's already false, another thread beat us to it.
                            if mining_clone
                                .compare_exchange(true, false, Ordering::AcqRel, Ordering::Relaxed)
                                .is_ok()
                            {
                                info!(
                                    "Mining paused by thread {} after finding a block.",
                                    thread_id
                                );
                                if let Err(e) = sender_clone.send(current_block.clone()) {
                                    error!(
                                        "Thread {} failed to send mined block through channel: {}",
                                        thread_id, e
                                    );
                                }
                            } else {
                                warn!("Thread {} found a block, but another thread was faster. Discarding.", thread_id);
                            }
                        }
                        // No block was found in this batch of attempts.
                        Ok((false, _attempts)) => {
                            total_hashes_clone
                                .fetch_add(max_attempts_per_call as u64, Ordering::Relaxed);
                        }
                        // An error occurred during mining.
                        Err(e) => {
                            error!("Error during mining in thread {}: {}", thread_id, e);
                            mining_clone.store(false, Ordering::Relaxed);
                        }
                    }
                }
            });
            handles.push(handle);
        }
        handles
    }

    /// Fetches a new template from the node and validates it.
    async fn fetch_and_validate_template(&self) -> Result<()> {
        match self.fetch_template().await {
            Ok(Some(template)) => {
                info!(
                    "Received template for block #{}. Target: {:x?}",
                    template.index, template.target
                );

                // Ask the node if the template we just received is still valid.
                match self.validate_template(Some(template.clone())).await {
                    Ok(true) => {
                        info!("Template is valid. Updating and ensuring mining is active.");
                        let mut current_template_guard = self.current_template.lock().await;
                        *current_template_guard = Some(template.clone());

                        self.mining.store(true, Ordering::Relaxed);

                        // ✅ Print estimated difficulty as a number (optional)
                        let diff = wisp_core::MAX_TARGET / template.target;
                        info!("Block difficulty: {}", diff);
                    }
                    Ok(false) => {
                        info!("Template was found to be stale by the node. Waiting for next fetch cycle.");
                    }
                    Err(e) => {
                        error!("Template validation failed: {}", e);
                        self.mining.store(false, Ordering::Relaxed);
                    }
                }
            }
            Ok(None) => {
                info!("No new template available from node.");
                self.mining.store(false, Ordering::Relaxed);
            }
            Err(e) => {
                error!("Failed to fetch template: {}", e);
                self.mining.store(false, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    /// Sends a `FetchTemplate` message to the node and waits for the `Template` response.
    async fn fetch_template(&self) -> Result<Option<Block>> {
        let message = Message::FetchTemplate(self.public_key.clone());
        let mut stream_lock = self.stream.lock().await;
        message
            .send_async(&mut *stream_lock)
            .await
            .map_err(|e| anyhow!("Failed to send FetchTemplate message: {}", e))?;
        match Message::receive_async(&mut *stream_lock)
            .await
            .map_err(|e| anyhow!("Failed to receive response for FetchTemplate: {}", e))?
        {
            Message::Template(template) => Ok(Some(template)),
            response => Err(anyhow!(
                "Unexpected message received when fetching template: {:?}",
                response
            )),
        }
    }

    /// Sends a `ValidateTemplate` message to the node to check if the current template is stale.
    /// A template can become stale if a new block is added to the chain by another miner.
    async fn validate_template(&self, new_template: Option<Block>) -> Result<bool> {
        let template_to_validate = new_template.or_else(|| {
            self.current_template
                .try_lock()
                .ok()
                .and_then(|g| g.clone())
        });

        if let Some(template) = template_to_validate {
            let message = Message::ValidateTemplate(template);
            let mut stream_lock = self.stream.lock().await;
            message
                .send_async(&mut *stream_lock)
                .await
                .map_err(|e| anyhow!("Failed to send ValidateTemplate message: {}", e))?;

            match Message::receive_async(&mut *stream_lock)
                .await
                .map_err(|e| anyhow!("Failed to receive response for ValidateTemplate: {}", e))?
            {
                Message::TemplateValidity(valid) => {
                    if !valid {
                        warn!("Current template is stale. Pausing mining and waiting for new template.");
                        self.mining.store(false, Ordering::Relaxed);
                        Ok(false)
                    } else {
                        trace!("Current template is still valid.");

                        if !self.mining.load(Ordering::Relaxed) {
                            info!("Template is valid, resuming mining.");
                            self.mining.store(true, Ordering::Relaxed);
                        }
                        Ok(true)
                    }
                }
                response => Err(anyhow!(
                    "Unexpected message received when validating template: {:?}",
                    response
                )),
            }
        } else {
            debug!("No template to validate.");
            self.mining.store(false, Ordering::Relaxed);
            Ok(false)
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

/// The application entry point for the miner.
#[tokio::main]
async fn main() -> Result<()> {
    // Use try_init to be robust in a workspace environment.
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("wisp_flame=info"),
    )
    .filter_module("symphonia_core::probe", log::LevelFilter::Warn)
    .filter_module("symphonia_bundle_mp3::demuxer", log::LevelFilter::Warn)
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

    // Spawn a task to handle SIGINT (Ctrl+C) for graceful shutdown.
    {
        let mining_flag = miner.mining.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                info!("Ctrl+C received — signaling mining threads to stop.");
                mining_flag.store(false, Ordering::Relaxed);
            }
        });
    }

    miner.run().await?;

    Ok(())
}
