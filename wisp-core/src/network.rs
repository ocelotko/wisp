use crate::{
    blockchain::{Block, BlockHeader},
    sha256::Hash,
    signatures::PublicKey,
    transactions::{OutPoint, Transaction, TransactionOutput},
    MAX_MESSAGE_SIZE,
};

use bincode::{config::standard as bincode_config, Decode, Encode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{Error as IoError, Read, Write};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Encode, Decode, Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub enum TransactionStatus {
    Pending,
    Confirmed { block_hash: Hash, block_index: u64 },
    Invalid,
    NotFound,
}

#[derive(Encode, Decode, Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct WalletTransactionInfo {
    pub transaction: Transaction,
    pub status: TransactionStatus,
    #[bincode(with_serde)]
    pub block_timestamp: Option<DateTime<Utc>>,
    pub block_index: Option<u64>,
}

#[derive(Encode, Decode, Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct WalletStateSnapshot {
    pub transactions: Vec<WalletTransactionInfo>,
    pub utxos: Vec<(OutPoint, TransactionOutput)>,
}

#[derive(Encode, Decode, Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub enum Message {
    // --- Wallet & Transaction Messages ---
    SubmitTransaction(Transaction),
    NewTransaction(Transaction),
    FetchWalletState(PublicKey),
    WalletState(WalletStateSnapshot),
    TransactionAcceptedConfirmation,
    TransactionRejected(Hash, String),
    FetchTransactionStatus(Hash),
    TransactionStatus {
        hash: Hash,
        status: TransactionStatus,
    },

    // --- Mining Messages ---
    FetchTemplate(PublicKey), // Miner requests a block template to work on.
    Template(Block),          // Node responds with a block template.
    NewTemplate(Block),       // Node pushes a new template to miners when the chain tip changes.
    ValidateTemplate(Block),  // Miner asks node to validate a found template before submitting.
    TemplateValidity(bool),   // Node responds with validity of the template.
    SubmitTemplate(Block),    // Miner submits a mined block.
    BlockSubmittedConfirmation, // Node confirms receipt and successful addition of the block.
    BlockRejected(String),    // Node rejects a submitted block.

    // --- Chain & Block Sync Messages ---
    NewBlock(Block),
    FetchBlock(u64),
    FetchBlockByHash(Hash),
    FetchLatestBlock,
    LatestBlock(Option<(Block, u64)>),

    // Headers-first synchronization messages
    GetBlockHeaders {
        from_index: u64,
        count: u32,
    }, // Request a sequence of block headers.
    BlockHeaders(Vec<BlockHeader>), // Response with the requested headers.

    FetchChainSegment(u64), // DEPRECATED: Prefer GetBlockHeaders and FetchBlock. Request blocks from a certain index onwards.
    ChainSegment(Vec<Block>), // DEPRECATED: Response with the requested blocks.

    // --- General & Peer Discovery Messages ---
    Ping,
    Pong,
    DiscoverNodes,
    NodeList(Vec<String>),
    FetchMempoolInfo,
    MempoolInfo(usize),
}

impl Message {
    /// Serializes the message into a byte vector using bincode.
    pub fn encode(&self) -> Result<Vec<u8>, IoError> {
        bincode::encode_to_vec(self, bincode_config()).map_err(|e| {
            IoError::new(
                std::io::ErrorKind::InvalidData,
                format!("Failed to encode message with bincode: {}", e),
            )
        })
    }

    /// Deserializes a byte slice into a `Message`.
    pub fn decode(data: &[u8]) -> Result<Self, IoError> {
        bincode::decode_from_slice(data, bincode_config())
            .map(|(msg, _)| msg)
            .map_err(|e| {
                IoError::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Failed to decode message with bincode: {}", e),
                )
            })
    }

    /// Sends the message over a synchronous stream, prepending its length.
    pub fn send(&self, stream: &mut impl Write) -> Result<(), IoError> {
        let bytes = self
            .encode()
            .map_err(|_| IoError::from(std::io::ErrorKind::InvalidData))?;
        let len = bytes.len() as u64;
        stream.write_all(&len.to_be_bytes())?;
        stream.write_all(&bytes)?;
        Ok(())
    }

    /// Receives a message from a synchronous stream, first reading the length.
    pub fn receive(stream: &mut impl Read) -> Result<Self, IoError> {
        let mut len_bytes = [0u8; 8];
        stream.read_exact(&mut len_bytes)?;
        let len = u64::from_be_bytes(len_bytes) as usize;

        if len > MAX_MESSAGE_SIZE {
            return Err(IoError::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Received message too large: {} bytes, max is {} bytes",
                    len, MAX_MESSAGE_SIZE
                ),
            ));
        }

        let mut data = vec![0u8; len];
        stream.read_exact(&mut data)?;

        Self::decode(&data).map_err(|_| IoError::from(std::io::ErrorKind::InvalidData))
    }

    /// Sends the message over an asynchronous stream, prepending its length.
    pub async fn send_async(&self, stream: &mut (impl AsyncWrite + Unpin)) -> Result<(), IoError> {
        let bytes = self
            .encode()
            .map_err(|_| IoError::from(std::io::ErrorKind::InvalidData))?;
        let len = bytes.len() as u64;
        stream.write_all(&len.to_be_bytes()).await?;
        stream.write_all(&bytes).await?;
        Ok(())
    }

    /// Receives a message from an asynchronous stream, first reading the length.
    pub async fn receive_async(stream: &mut (impl AsyncRead + Unpin)) -> Result<Self, IoError> {
        let mut len_bytes = [0u8; 8];
        stream.read_exact(&mut len_bytes).await?;
        let len = u64::from_be_bytes(len_bytes) as usize;

        if len > MAX_MESSAGE_SIZE {
            return Err(IoError::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Received message too large: {} bytes, max is {} bytes",
                    len, MAX_MESSAGE_SIZE
                ),
            ));
        }

        let mut data = Vec::with_capacity(len);
        let mut stream_reader = stream.take(len as u64);
        stream_reader.read_to_end(&mut data).await?;

        if data.len() != len {
            return Err(IoError::new(
                std::io::ErrorKind::UnexpectedEof,
                "Failed to read the full message body",
            ));
        }

        Self::decode(&data)
    }
}
