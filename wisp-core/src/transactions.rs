use crate::sha256::Hash;
use crate::{
    currency::Amount,
    signatures::PublicKey,
    signatures::{PrivateKey, Signature},
};
use anyhow::Result;
use bincode::{Decode, Encode};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::hash::Hash as StdHash;

#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, StdHash)]
pub enum Script {
    /// Classic (P2PK)
    Classic(PublicKey),
    /// Shadow (P2PKH)
    Shadow(Hash),
    /// Shadow-Script (P2SH)
    ShadowScript(Hash),
    /// Aurora (P2WPKH)
    Aurora(Hash),
    /// Aurora-Script (P2WSH)
    AuroraScript(Hash),
}

impl Script {
    pub fn is_relevant_to(
        &self,
        pubkey: &PublicKey,
        pk_hash_bytes: &[u8],
        known_hashes: &HashSet<Hash>,
    ) -> bool {
        match self {
            Script::Classic(pk) => pk == pubkey,
            Script::Shadow(h) | Script::Aurora(h) => h.as_bytes()[..20] == pk_hash_bytes[..20],
            Script::ShadowScript(h) | Script::AuroraScript(h) => known_hashes.contains(h),
        }
    }
}

impl Script {
    pub fn new_shadow(pk: &PublicKey) -> Self {
        let hash_bytes = crate::address::Address::hash160(pk);
        let mut h = [0u8; 32];
        h[..20].copy_from_slice(&hash_bytes);
        Script::Shadow(Hash::from_bytes(&h))
    }

    pub fn new_aurora(pk: &PublicKey) -> Self {
        let hash_bytes = crate::address::Address::hash160(pk);
        let mut h = [0u8; 32];
        h[..20].copy_from_slice(&hash_bytes);
        Script::Aurora(Hash::from_bytes(&h))
    }
}

/// Represents a transaction, which is a collection of inputs and outputs.
#[derive(Encode, Decode, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Transaction {
    pub inputs: Vec<TransactionInput>,
    pub outputs: Vec<TransactionOutput>,
}

/// A pointer to a specific transaction output.
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
    pub public_key: Option<PublicKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redeem_script: Option<Vec<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub coinbase_data: Option<Vec<u8>>,
}

#[derive(Encode, Decode, Clone, Debug, Serialize, Deserialize, PartialEq, Eq, StdHash, Default)]
pub struct TransactionOutput {
    pub value: Amount,
    pub script: Script,
}

impl Default for Script {
    fn default() -> Self {
        Script::Classic(PublicKey::default())
    }
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
        match &self.script {
            Script::Classic(pk) => pk.update_hasher(hasher),
            Script::Shadow(h) => h.update_hasher(hasher),
            Script::ShadowScript(h) => h.update_hasher(hasher),
            Script::Aurora(h) => h.update_hasher(hasher),
            Script::AuroraScript(h) => h.update_hasher(hasher),
        }
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
            if let Some(pk) = &input.public_key {
                pk.update_hasher(hasher);
            }
            if let Some(rs) = &input.redeem_script {
                hasher.update(rs);
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
                    public_key: Some(private_key.public_key()),
                    redeem_script: None,
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
                    public_key: Some(private_key.public_key()),
                    redeem_script: None,
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
