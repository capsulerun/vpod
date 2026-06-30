use rand_core::OsRng;
use x25519_dalek::{PublicKey, StaticSecret};
use p256::ecdsa::{Signature as P256Signature, VerifyingKey as P256VerifyingKey};
use ed25519_dalek::{Signature as Ed25519Signature, VerifyingKey as Ed25519VerifyingKey};

use rsa::pkcs1v15::{Signature as RsaSignature, VerifyingKey as RsaVerifyingKey};
use rsa::pkcs8::DecodePublicKey;
use rsa::signature::Verifier;
use sha2::Sha256;

use crate::uart::Uart;



const OP_X25519_KEYGEN: u8 = 0x01;
const OP_X25519_DERIVE: u8 = 0x02;
const OP_ECDSA_P256_VERIFY: u8 = 0x03;
const OP_RSA_VERIFY: u8 = 0x04;
const OP_ED25519_VERIFY: u8 = 0x05;

const STATUS_OK: u8 = 0x00;
const STATUS_ERR: u8 = 0x01;

pub struct CryptoHandler {
    request_buf: Vec<u8>,
    expected_len: Option<usize>,
}

impl CryptoHandler {
    pub fn new() -> Self {
        Self {
            request_buf: Vec::new(),
            expected_len: None,
        }
    }

    pub fn process(&mut self, uart: &Uart) {
        let tx_data = uart.drain_tx();
        if tx_data.is_empty() {
            return;
        }

        self.request_buf.extend_from_slice(&tx_data);

        loop {
            if self.expected_len.is_none() {
                if self.request_buf.len() < 4 {
                    return;
                }
                let len = u32::from_le_bytes(
                    self.request_buf[0..4].try_into().unwrap(),
                ) as usize;
                self.expected_len = Some(len);
            }

            let total = 4 + self.expected_len.unwrap();
            if self.request_buf.len() < total {
                return;
            }

            let request: Vec<u8> = self.request_buf.drain(..total).collect();
            self.expected_len = None;

            let response = self.handle_request(&request[4..]);
            let resp_len = (response.len() as u32).to_le_bytes();

            for &b in resp_len.iter().chain(response.iter()) {
                uart.push_rx(b);
            }
        }
    }

    fn handle_request(&self, data: &[u8]) -> Vec<u8> {
        if data.is_empty() {
            return vec![STATUS_ERR];
        }

        match data[0] {
            OP_X25519_KEYGEN => self.x25519_keygen(),
            OP_X25519_DERIVE => self.x25519_derive(&data[1..]),
            OP_ECDSA_P256_VERIFY => self.ecdsa_p256_verify(&data[1..]),
            OP_RSA_VERIFY => self.rsa_verify(&data[1..]),
            OP_ED25519_VERIFY => self.ed25519_verify(&data[1..]),
            _ => vec![STATUS_ERR],
        }
    }

    fn x25519_keygen(&self) -> Vec<u8> {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        let mut out = vec![STATUS_OK];

        out.extend_from_slice(secret.as_bytes());
        out.extend_from_slice(public.as_bytes());
        out
    }

    fn x25519_derive(&self, data: &[u8]) -> Vec<u8> {
        if data.len() < 64 {
            return vec![STATUS_ERR];
        }
        let private_bytes: [u8; 32] = data[0..32].try_into().unwrap();
        let peer_public_bytes: [u8; 32] = data[32..64].try_into().unwrap();

        let secret = StaticSecret::from(private_bytes);
        let peer_public = PublicKey::from(peer_public_bytes);
        let shared = secret.diffie_hellman(&peer_public);

        let mut out = vec![STATUS_OK];
        out.extend_from_slice(shared.as_bytes());
        out
    }

    fn ecdsa_p256_verify(&self, data: &[u8]) -> Vec<u8> {
        if data.len() < 4 {
            return vec![STATUS_ERR];
        }

        let pubkey_len = u16::from_le_bytes(data[0..2].try_into().unwrap()) as usize;
        let sig_len = u16::from_le_bytes(data[2..4].try_into().unwrap()) as usize;
        let header = 4;

        if data.len() < header + pubkey_len + sig_len + 32 {
            return vec![STATUS_ERR];
        }

        let pubkey_bytes = &data[header..header + pubkey_len];
        let sig_bytes = &data[header + pubkey_len..header + pubkey_len + sig_len];
        let msg_hash = &data[header + pubkey_len + sig_len..header + pubkey_len + sig_len + 32];

        let vk = match P256VerifyingKey::from_sec1_bytes(pubkey_bytes) {
            Ok(k) => k,
            Err(_) => return vec![STATUS_ERR],
        };

        let sig = match P256Signature::from_der(sig_bytes) {
            Ok(s) => s,
            Err(_) => return vec![STATUS_ERR],
        };

        match vk.verify(msg_hash, &sig) {
            Ok(()) => vec![STATUS_OK],
            Err(_) => vec![STATUS_ERR],
        }
    }

    fn rsa_verify(&self, data: &[u8]) -> Vec<u8> {
        if data.len() < 6 {
            return vec![STATUS_ERR];
        }

        let pubkey_len = u16::from_le_bytes(data[0..2].try_into().unwrap()) as usize;
        let sig_len = u16::from_le_bytes(data[2..4].try_into().unwrap()) as usize;
        let msg_len = u16::from_le_bytes(data[4..6].try_into().unwrap()) as usize;
        let header = 6;

        if data.len() < header + pubkey_len + sig_len + msg_len {
            return vec![STATUS_ERR];
        }

        let pubkey_bytes = &data[header..header + pubkey_len];
        let sig_bytes = &data[header + pubkey_len..header + pubkey_len + sig_len];
        let msg = &data[header + pubkey_len + sig_len..header + pubkey_len + sig_len + msg_len];

        let rsa_pub = match rsa::RsaPublicKey::from_public_key_der(pubkey_bytes) {
            Ok(k) => k,
            Err(_) => return vec![STATUS_ERR],
        };

        let verifying_key = RsaVerifyingKey::<Sha256>::new_unprefixed(rsa_pub);
        let signature = match RsaSignature::try_from(sig_bytes) {
            Ok(s) => s,
            Err(_) => return vec![STATUS_ERR],
        };

        match verifying_key.verify(msg, &signature) {
            Ok(()) => vec![STATUS_OK],
            Err(_) => vec![STATUS_ERR],
        }
    }

    fn ed25519_verify(&self, data: &[u8]) -> Vec<u8> {
        if data.len() < 4 {
            return vec![STATUS_ERR];
        }

        let sig_len = 64;
        let pubkey_len = 32;
        let msg_len = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
        let header = 4;

        if data.len() < header + pubkey_len + sig_len + msg_len {
            return vec![STATUS_ERR];
        }

        let pubkey_bytes: [u8; 32] = data[header..header + 32].try_into().unwrap();
        let sig_bytes: [u8; 64] = data[header + 32..header + 96].try_into().unwrap();
        let msg = &data[header + 96..header + 96 + msg_len];

        let vk = match Ed25519VerifyingKey::from_bytes(&pubkey_bytes) {
            Ok(k) => k,
            Err(_) => return vec![STATUS_ERR],
        };

        let sig = Ed25519Signature::from_bytes(&sig_bytes);

        match vk.verify(msg, &sig) {
            Ok(()) => vec![STATUS_OK],
            Err(_) => vec![STATUS_ERR],
        }
    }
}
