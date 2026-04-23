use crate::sha256::{hash, Hash};
use crate::signatures::PublicKey;
use crate::transactions::Script;
use anyhow::{anyhow, Result};
use ripemd::{Digest, Ripemd160};

const WISP32_ALPHABET: &[u8] = b"acdefghjkmnpqrstuvwxyz0123456789";

pub struct Address;

impl Address {
    pub fn hash160(pk: &PublicKey) -> Vec<u8> {
        let sha_hash = crate::sha256::hash(pk);
        let mut ripemd = Ripemd160::new();
        ripemd.update(sha_hash.as_bytes());
        ripemd.finalize().to_vec()
    }

    pub fn encode(script: &Script) -> String {
        match script {
            Script::Classic(pk) => {
                let mut data = vec![0x57];
                data.extend_from_slice(&pk.to_bytes());
                bs58::encode(data).with_check().into_string()
            }
            Script::Shadow(h) | Script::ShadowScript(h) => {
                let mut data = vec![0x3F];
                data.extend_from_slice(&h.as_bytes()[..20]);
                bs58::encode(data).with_check().into_string()
            }
            Script::Aurora(h) | Script::AuroraScript(h) => {
                let prefix = "w+";

                let mut to_checksum = if matches!(script, Script::Aurora(_)) {
                    h.as_bytes()[..20].to_vec()
                } else {
                    h.as_bytes().to_vec()
                };

                let check_hash = hash(&to_checksum);
                to_checksum.extend_from_slice(&check_hash.as_bytes()[..4]);

                let encoded = Self::wisp32_encode(&to_checksum);
                format!("{}{}", prefix, encoded)
            }
        }
    }

    pub fn decode(address: &str) -> Result<Script> {
        if address.starts_with("w+") {
            let payload_str = &address[2..];
            let decoded = Self::wisp32_decode(payload_str)?;
            if decoded.len() < 4 {
                return Err(anyhow!("Aurora address too short"));
            }

            let (data, checksum) = decoded.split_at(decoded.len() - 4);
            let check_hash = hash(data);
            if &check_hash.as_bytes()[..4] != checksum {
                return Err(anyhow!("Aurora address checksum failed"));
            }

            if data.len() == 20 {
                let mut h_bytes = [0u8; 32];
                h_bytes[..20].copy_from_slice(data);
                Ok(Script::Aurora(Hash::from_bytes(&h_bytes)))
            } else if data.len() == 32 {
                Ok(Script::AuroraScript(Hash::from_bytes(data.try_into()?)))
            } else {
                Err(anyhow!("Invalid Aurora payload length: {}", data.len()))
            }
        } else if let Ok(decoded) = bs58::decode(address).with_check(None).into_vec() {
            if decoded.is_empty() {
                return Err(anyhow!("Empty address payload"));
            }
            match decoded[0] {
                0x57 => {
                    if decoded.len() != 34 {
                        return Err(anyhow!("Invalid Classic address length"));
                    }
                    let pk = k256::ecdsa::VerifyingKey::from_sec1_bytes(&decoded[1..])
                        .map(PublicKey)
                        .map_err(|e| anyhow!("Invalid public key: {}", e))?;
                    Ok(Script::Classic(pk))
                }
                0x3F => {
                    if decoded.len() != 21 {
                        return Err(anyhow!("Invalid Shadow address length"));
                    }
                    let mut h_bytes = [0u8; 32];
                    h_bytes[..20].copy_from_slice(&decoded[1..21]);
                    Ok(Script::Shadow(Hash::from_bytes(&h_bytes)))
                }
                v => Err(anyhow!("Unknown address version byte: 0x{:02x}", v)),
            }
        } else {
            // Fallback for raw hex public keys
            use std::str::FromStr;
            let pk = PublicKey::from_str(address)?;
            Ok(Script::Classic(pk))
        }
    }

    fn wisp32_decode(s: &str) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut bit_accumulator: u32 = 0;
        let mut bit_count: u8 = 0;
        for c in s.chars() {
            let val = WISP32_ALPHABET
                .iter()
                .position(|&x| x == c as u8)
                .ok_or_else(|| anyhow!("Invalid character in Wisp32"))?
                as u32;
            bit_accumulator = (bit_accumulator << 5) | val;
            bit_count += 5;
            if bit_count >= 8 {
                bit_count -= 8;
                bytes.push((bit_accumulator >> bit_count) as u8);
            }
        }
        Ok(bytes)
    }

    fn wisp32_encode(data: &[u8]) -> String {
        let mut result = String::new();
        let mut bit_accumulator: u32 = 0;
        let mut bit_count: u8 = 0;

        for &byte in data {
            bit_accumulator = (bit_accumulator << 8) | (byte as u32);
            bit_count += 8;
            while bit_count >= 5 {
                bit_count -= 5;
                let index = (bit_accumulator >> bit_count) & 0x1F;
                result.push(WISP32_ALPHABET[index as usize] as char);
            }
        }
        if bit_count > 0 {
            let index = (bit_accumulator << (5 - bit_count)) & 0x1F;
            result.push(WISP32_ALPHABET[index as usize] as char);
        }
        result
    }
}
