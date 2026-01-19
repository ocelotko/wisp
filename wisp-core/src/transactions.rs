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
    /// Inputs spending previous outputs.
    pub inputs: Vec<TransactionInput>,
    /// New outputs created by this transaction.
    pub outputs: Vec<TransactionOutput>,
}

/// A pointer to a specific transaction output.
#[derive(
    Encode, Decode, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, StdHash, Copy, Default,
)]
pub struct OutPoint {
    /// The hash of the transaction containing the output.
    pub txid: Hash,
    /// The index of the output in that transaction's output list.
    pub vout: u32,
}

impl fmt::Display for OutPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.txid, self.vout)
    }
}

#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct TransactionInput {
    /// The output being spent.
    pub outpoint: OutPoint,
    /// The signature proving ownership of the output.
    pub signature: Option<Signature>,
    /// Data for coinbase transactions (e.g., block height, extra nonce).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub coinbase_data: Option<Vec<u8>>,
}

#[derive(Encode, Decode, Clone, Debug, Serialize, Deserialize, PartialEq, Eq, StdHash, Default)]
pub struct TransactionOutput {
    /// The amount of currency being transferred.
    pub value: Amount,
    /// The public key of the recipient (locking script).
    pub pubkey: PublicKey,
}

use crate::sha256::{hash, witness_hash, Hashable, WitnessHashable};
use sha2::{Digest, Sha256};

impl Hashable for OutPoint {
    fn update_hasher(&self, hasher: &mut Sha256) {
        hasher.update(&self.txid.as_bytes());
        hasher.update(&self.vout.to_be_bytes());
    }
}

impl Hashable for TransactionOutput {
    fn update_hasher(&self, hasher: &mut Sha256) {
        self.value.update_hasher(hasher);
        self.pubkey.update_hasher(hasher);
    }
}

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
    pub fn new(inputs: Vec<TransactionInput>, outputs: Vec<TransactionOutput>) -> Self {
        Transaction { inputs, outputs }
    }

    pub fn is_coinbase(&self) -> bool {
        self.inputs.len() == 1 && self.inputs[0].outpoint.txid == Hash::zero()
    }

    pub fn new_signed_from_utxos(
        outpoints_to_spend: &[OutPoint],
        outputs: Vec<TransactionOutput>,
        private_key: &PrivateKey,
    ) -> Result<Self, anyhow::Error> {
        let tx_to_sign = Transaction {
            inputs: outpoints_to_spend
                .iter()
                .map(|outpoint| TransactionInput {
                    outpoint: *outpoint,
                    signature: None,
                    coinbase_data: None,
                })
                .collect(),
            outputs: outputs.clone(),
        };

        let tx_hash_to_sign = hash(&tx_to_sign);
        let signature = Signature::sign_transaction_hash(&tx_hash_to_sign, private_key);

        Ok(Transaction {
            inputs: outpoints_to_spend
                .iter()
                .map(|outpoint| TransactionInput {
                    outpoint: *outpoint,
                    signature: Some(signature.clone()),
                    coinbase_data: None,
                })
                .collect(),
            outputs,
        })
    }

    pub fn txid(&self) -> Result<Hash, anyhow::Error> {
        Ok(hash(self))
    }

    pub fn wtxid(&self) -> Result<Hash, anyhow::Error> {
        Ok(witness_hash(self))
    }
}
