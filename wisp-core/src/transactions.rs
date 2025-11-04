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

/// An "OutPoint" is a pointer to a specific transaction output. It consists of the hash of the transaction
/// that created the output and the output's index within that transaction (`vout`).
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

#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct TransactionInput {
    pub outpoint: OutPoint,
    pub signature: Option<Signature>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub coinbase_data: Option<Vec<u8>>,
}

/// A transaction output, which creates new spendable value. It specifies the amount, the public key
/// that can spend it (the "lock script"), and an optional message.
#[derive(Encode, Decode, Clone, Debug, Serialize, Deserialize, PartialEq, Eq, StdHash, Default)]
pub struct TransactionOutput {
    pub value: Amount,
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

    /// Checks if the transaction is a coinbase transaction.
    pub fn is_coinbase(&self) -> bool {
        self.inputs.len() == 1 && self.inputs[0].outpoint.txid == Hash::zero()
    }

    pub fn new_signed_from_utxos(
        outpoints_to_spend: &[OutPoint],
        outputs: Vec<TransactionOutput>,
        private_key: &PrivateKey,
    ) -> Result<Self, anyhow::Error> {
        let inputs_for_signing = outpoints_to_spend
            .iter()
            .map(|outpoint| TransactionInput {
                outpoint: *outpoint,
                signature: None,
                coinbase_data: None,
            })
            .collect();

        let tx_to_sign = Transaction {
            inputs: inputs_for_signing,
            outputs: outputs.clone(),
        };

        let tx_hash_to_sign = hash(&tx_to_sign);

        let signature = Signature::sign_transaction_hash(&tx_hash_to_sign, private_key);

        let inputs_with_signature = outpoints_to_spend
            .iter()
            .map(|outpoint| TransactionInput {
                outpoint: *outpoint,
                signature: Some(signature.clone()),
                coinbase_data: None,
            })
            .collect();

        Ok(Transaction {
            inputs: inputs_with_signature,
            outputs,
        })
    }

    /// The transaction ID (txid) is the hash of the signable components of the transaction.
    pub fn txid(&self) -> Result<Hash, anyhow::Error> {
        Ok(hash(self))
    }

    /// The witness transaction ID (wtxid) is the hash of the transaction including witness data (signatures).
    /// This is used for the Merkle Root calculation to prevent malleability.
    pub fn wtxid(&self) -> Result<Hash, anyhow::Error> {
        Ok(witness_hash(self))
    }
}
