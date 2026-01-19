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
use std::{
    convert::TryFrom,
    io::{Error as IoError, Read, Write},
};
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
    FetchTemplate(PublicKey, Option<String>),
    Template(Block),
    NewTemplate(Block),
    ValidateTemplate(Block),
    TemplateValidity(bool),
    SubmitTemplate(PublicKey, Block, Option<String>),
    BlockSubmittedConfirmation,
    BlockRejected(String),

    // --- Chain & Block Sync Messages ---
    NewBlock(Block),
    FetchBlock(u64),
    FetchBlockByHash(Hash),
    FetchBlockInfo(u64),
    BlockInfo(Option<Block>),
    FetchLatestBlock,
    LatestBlock(Option<(Block, u64)>),

    // Headers-first synchronization messages
    GetBlockHeaders {
        from_index: u64,
        count: u32,
    },
    BlockHeaders(Vec<BlockHeader>),

    // --- General & Peer Discovery Messages ---
    Hello(String),
    Ping,
    Pong,
    DiscoverNodes,
    NodeList(Vec<String>),
    FetchMempoolInfo,
    MempoolInfo(usize),
}

impl Message {
    pub fn encode(&self) -> Result<Vec<u8>, IoError> {
        bincode::encode_to_vec(self, bincode_config()).map_err(|e| {
            IoError::new(
                std::io::ErrorKind::InvalidData,
                format!("Failed to encode message with bincode: {}", e),
            )
        })
    }

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

    pub fn send(&self, stream: &mut impl Write) -> Result<(), IoError> {
        let bytes = self.encode()?;
        let len = bytes.len() as u64;

        if len == 0 {
            return Err(IoError::new(
                std::io::ErrorKind::InvalidData,
                "Cannot send a zero-length message",
            ));
        }
        if len as usize > MAX_MESSAGE_SIZE {
            return Err(IoError::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Message too large to send: {} bytes > max {} bytes",
                    len, MAX_MESSAGE_SIZE
                ),
            ));
        }

        stream.write_all(&len.to_be_bytes())?;
        stream.write_all(&bytes)?;
        Ok(())
    }

    pub fn receive(stream: &mut impl Read) -> Result<Self, IoError> {
        let mut len_bytes = [0u8; 8];
        stream.read_exact(&mut len_bytes)?;
        let len_u64 = u64::from_be_bytes(len_bytes);

        if len_u64 == 0 {
            return Err(IoError::new(
                std::io::ErrorKind::InvalidData,
                "Received zero-length message header",
            ));
        }

        let len = usize::try_from(len_u64).map_err(|_| {
            IoError::new(
                std::io::ErrorKind::InvalidData,
                "Message length exceeds platform's usize capacity",
            )
        })?;

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

        Self::decode(&data)
    }

    pub async fn send_async(&self, stream: &mut (impl AsyncWrite + Unpin)) -> Result<(), IoError> {
        let bytes = self.encode()?;
        let len = bytes.len() as u64;

        if len == 0 {
            return Err(IoError::new(
                std::io::ErrorKind::InvalidData,
                "Cannot send a zero-length message",
            ));
        }
        if len as usize > MAX_MESSAGE_SIZE {
            return Err(IoError::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Message too large to send: {} bytes > max {} bytes",
                    len, MAX_MESSAGE_SIZE
                ),
            ));
        }

        stream.write_all(&len.to_be_bytes()).await?;
        stream.write_all(&bytes).await?;
        Ok(())
    }

    pub async fn receive_async(stream: &mut (impl AsyncRead + Unpin)) -> Result<Self, IoError> {
        let mut len_bytes = [0u8; 8];
        stream.read_exact(&mut len_bytes).await?;
        let len_u64 = u64::from_be_bytes(len_bytes);

        if len_u64 == 0 {
            return Err(IoError::new(
                std::io::ErrorKind::InvalidData,
                "Received zero-length message header",
            ));
        }

        let len = usize::try_from(len_u64).map_err(|_| {
            IoError::new(
                std::io::ErrorKind::InvalidData,
                "Message length exceeds platform's usize capacity",
            )
        })?;

        if len > MAX_MESSAGE_SIZE {
            return Err(IoError::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Received message header for too large a message: {} bytes, max is {} bytes",
                    len, MAX_MESSAGE_SIZE
                ),
            ));
        }

        let mut data = vec![0u8; len];
        stream.read_exact(&mut data).await?;

        Self::decode(&data)
    }
}
