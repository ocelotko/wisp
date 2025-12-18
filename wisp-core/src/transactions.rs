use crate::sha256::Hash;
use crate::{
    currency::Amount,
    signatures::PublicKey,
    signatures::{PrivateKey, Signature},
};
use anyhow::Result;
use bincode::{Decode, Encode};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::hash::Hash as StdHash;

/// Represents a transaction, which is a collection of inputs and outputs.
#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Transaction {
    pub inputs: Vec<TransactionInput>,
    pub outputs: Vec<TransactionOutput>,
}

/// A pointer to a specific transaction output.
///
/// An `OutPoint` uniquely identifies a spendable output by referencing the hash
/// of the transaction that created it (`txid`) and its index within that
/// transaction's outputs list (`vout`).
#[derive(
    Encode, Decode, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, StdHash, Copy, Default,
)]
pub struct OutPoint {
    pub txid: Hash,
    pub vout: u32,
}

impl fmt::Display for OutPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.txid, self.vout)
    }
}

/// Represents an input to a transaction, which spends a previous transaction's output.
#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct TransactionInput {
    pub outpoint: OutPoint,
    pub signature: Option<Signature>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub coinbase_data: Option<Vec<u8>>,
}

/// A transaction output, which creates new spendable value on the blockchain.
///
/// It specifies the `value` (amount) and the `pubkey` (script) that is
/// required to spend this output in a future transaction.
#[derive(Encode, Decode, Clone, Debug, Serialize, Deserialize, PartialEq, Eq, StdHash, Default)]
pub struct TransactionOutput {
    pub value: Amount,
    pub pubkey: PublicKey,
}

use crate::sha256::{hash, witness_hash, Hashable, WitnessHashable};
use sha2::{Digest, Sha256};

/// Implements `Hashable` for `OutPoint` to include it in the transaction hash.
impl Hashable for OutPoint {
    fn update_hasher(&self, hasher: &mut Sha256) {
        hasher.update(&self.txid.as_bytes());
        hasher.update(&self.vout.to_be_bytes());
    }
}

/// Implements `Hashable` for `TransactionOutput` to include it in the transaction hash.
impl Hashable for TransactionOutput {
    fn update_hasher(&self, hasher: &mut Sha256) {
        self.value.update_hasher(hasher);
        self.pubkey.update_hasher(hasher);
    }
}

/// Implements `Hashable` for `Transaction` to define how a `txid` is calculated.
impl Hashable for Transaction {
    fn update_hasher(&self, hasher: &mut Sha256) {
        for input in &self.inputs {
            input.outpoint.update_hasher(hasher);
            if let Some(coinbase) = &input.coinbase_data {
                hasher.update(coinbase);
            }
        }
        for output in &self.outputs {
            output.update_hasher(hasher);
        }
    }
}

/// Implements `WitnessHashable` for `Transaction` to define how a `wtxid` is calculated.
impl WitnessHashable for Transaction {
    fn update_witness_hasher(&self, hasher: &mut Sha256) {
        for input in &self.inputs {
            input.outpoint.update_hasher(hasher);
            if let Some(sig) = &input.signature {
                hasher.update(sig.0.to_bytes());
            }
        }
        for output in &self.outputs {
            output.update_hasher(hasher);
        }
    }
}

impl Transaction {
    /// Creates a new transaction from a vector of inputs and outputs.
    pub fn new(inputs: Vec<TransactionInput>, outputs: Vec<TransactionOutput>) -> Self {
        Transaction { inputs, outputs }
    }

    /// Checks if the transaction is a coinbase transaction.
    pub fn is_coinbase(&self) -> bool {
        self.inputs.len() == 1 && self.inputs[0].outpoint.txid == Hash::zero()
    }

    /// Creates and signs a new transaction from a set of UTXOs to be spent.
    ///
    /// This function is designed for a simple wallet where a single private key controls
    /// all input UTXOs. It creates a transaction, signs it with the provided key,
    /// and places the signature in all input fields.
    pub fn new_signed_from_utxos(
        outpoints_to_spend: &[OutPoint],
        outputs: Vec<TransactionOutput>,
        private_key: &PrivateKey,
    ) -> Result<Self, anyhow::Error> {
        // 1. Create the transaction with placeholder (empty) inputs to generate the hash to be signed.
        let tx_to_sign = Transaction {
            inputs: outpoints_to_spend
                .iter()
                .map(|outpoint| TransactionInput {
                    outpoint: *outpoint,
                    signature: None, // Signature is None for hashing
                    coinbase_data: None,
                })
                .collect(),
            outputs: outputs.clone(),
        };

        // 2. Calculate the transaction ID (txid), which is what gets signed.
        let tx_hash_to_sign = hash(&tx_to_sign);

        // 3. Create a single signature for the entire transaction.
        let signature = Signature::sign_transaction_hash(&tx_hash_to_sign, private_key);

        // 4. Create the final transaction by applying the same signature to all inputs.
        // This is a simplification suitable for single-key wallets.
        Ok(Transaction {
            inputs: outpoints_to_spend
                .iter()
                .map(|outpoint| TransactionInput {
                    outpoint: *outpoint,
                    signature: Some(signature.clone()), // Clone the signature for each input
                    coinbase_data: None,
                })
                .collect(),
            outputs,
        })
    }

    /// Calculates the transaction ID (`txid`).
    ///
    /// The `txid` is the double-SHA256 hash of the transaction's signable components
    /// (i.e., excluding witness data like signatures).
    pub fn txid(&self) -> Result<Hash, anyhow::Error> {
        Ok(hash(self))
    }

    /// Calculates the witness transaction ID (`wtxid`).
    /// The `wtxid` is the hash of the transaction including witness data (signatures).
    pub fn wtxid(&self) -> Result<Hash, anyhow::Error> {
        Ok(witness_hash(self))
    }
}
