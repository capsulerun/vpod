use rand_core::OsRng;
use x25519_dalek::{PublicKey, StaticSecret};
use p256::ecdsa::{Signature as P256Signature, VerifyingKey as P256VerifyingKey};
use ed25519_dalek::{Signature as Ed25519Signature, VerifyingKey as Ed25519VerifyingKey};

use rsa::pkcs1v15::{Signature as RsaSignature, VerifyingKey as RsaVerifyingKey};
use rsa::signature::Verifier;
use rsa::{BigUint, RsaPublicKey};
use sha1::Sha1;
use sha2::{Sha256, Sha384, Sha512};

use aes_gcm::{Aes128Gcm, Aes256Gcm, KeyInit, Nonce};
use aes_gcm::aead::Aead;

const OP_X25519_KEYGEN: u8 = 0x01;
const OP_X25519_DERIVE: u8 = 0x02;
const OP_ECDSA_P256_VERIFY: u8 = 0x03;
const OP_RSA_VERIFY: u8 = 0x04;
const OP_ED25519_VERIFY: u8 = 0x05;
const OP_AES_GCM_ENCRYPT: u8 = 0x06;
const OP_AES_GCM_DECRYPT: u8 = 0x07;

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

    pub fn process_bytes(&mut self, input: &[u8]) -> Vec<u8> {
        self.request_buf.extend_from_slice(input);
        let mut output = Vec::new();

        loop {
            if self.expected_len.is_none() {
                if self.request_buf.len() < 4 {
                    break;
                }
                let len = u32::from_le_bytes(
                    self.request_buf[0..4].try_into().unwrap(),
                ) as usize;
                self.expected_len = Some(len);
            }

            let total = 4 + self.expected_len.unwrap();
            if self.request_buf.len() < total {
                break;
            }

            let request: Vec<u8> = self.request_buf.drain(..total).collect();
            self.expected_len = None;

            let response = self.handle_request(&request[4..]);
            output.extend_from_slice(&(response.len() as u32).to_le_bytes());
            output.extend_from_slice(&response);
        }

        output
    }

    pub fn handle_request(&self, data: &[u8]) -> Vec<u8> {
        if data.is_empty() {
            return vec![STATUS_ERR];
        }

        let result = match data[0] {
            OP_X25519_KEYGEN => self.x25519_keygen(),
            OP_X25519_DERIVE => self.x25519_derive(&data[1..]),
            OP_ECDSA_P256_VERIFY => self.ecdsa_p256_verify(&data[1..]),
            OP_RSA_VERIFY => self.rsa_verify(&data[1..]),
            OP_ED25519_VERIFY => self.ed25519_verify(&data[1..]),
            OP_AES_GCM_ENCRYPT => self.aes_gcm_encrypt(&data[1..]),
            OP_AES_GCM_DECRYPT => self.aes_gcm_decrypt(&data[1..]),
            _ => vec![STATUS_ERR],
        };
        eprintln!("[crypto] op=0x{:02x} req_len={} result={}", data[0], data.len(), if result[0] == STATUS_OK { "OK" } else { "ERR" });
        result
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
        // Wire: [hash:1][pubkey_len:2][sig_len:2][msg_len:2][pubkey_sec1][sig_der][msg]
        if data.len() < 7 {
            return vec![STATUS_ERR];
        }

        let _hash_id = data[0];
        let pubkey_len = u16::from_le_bytes(data[1..3].try_into().unwrap()) as usize;
        let sig_len = u16::from_le_bytes(data[3..5].try_into().unwrap()) as usize;
        let msg_len = u16::from_le_bytes(data[5..7].try_into().unwrap()) as usize;
        let header = 7;

        if data.len() < header + pubkey_len + sig_len + msg_len {
            return vec![STATUS_ERR];
        }

        let pubkey_bytes = &data[header..header + pubkey_len];
        let sig_bytes = &data[header + pubkey_len..header + pubkey_len + sig_len];
        let msg = &data[header + pubkey_len + sig_len..header + pubkey_len + sig_len + msg_len];

        let vk = match P256VerifyingKey::from_sec1_bytes(pubkey_bytes) {
            Ok(k) => k,
            Err(_) => return vec![STATUS_ERR],
        };

        let sig = match P256Signature::from_der(sig_bytes) {
            Ok(s) => s,
            Err(_) => return vec![STATUS_ERR],
        };

        // Verifier trait hashes the raw message internally with SHA-256
        match vk.verify(msg, &sig) {
            Ok(()) => vec![STATUS_OK],
            Err(_) => vec![STATUS_ERR],
        }
    }

    fn rsa_verify(&self, data: &[u8]) -> Vec<u8> {
        // Wire: [hash:1][n_len:2][e_len:2][sig_len:2][msg_len:2][n][e][sig][msg]
        if data.len() < 9 {
            return vec![STATUS_ERR];
        }

        let hash_id = data[0];
        let n_len = u16::from_le_bytes(data[1..3].try_into().unwrap()) as usize;
        let e_len = u16::from_le_bytes(data[3..5].try_into().unwrap()) as usize;
        let sig_len = u16::from_le_bytes(data[5..7].try_into().unwrap()) as usize;
        let msg_len = u16::from_le_bytes(data[7..9].try_into().unwrap()) as usize;
        let header = 9;

        if data.len() < header + n_len + e_len + sig_len + msg_len {
            return vec![STATUS_ERR];
        }

        let n_bytes = &data[header..header + n_len];
        let e_bytes = &data[header + n_len..header + n_len + e_len];
        let sig_bytes = &data[header + n_len + e_len..header + n_len + e_len + sig_len];
        let msg = &data[header + n_len + e_len + sig_len..header + n_len + e_len + sig_len + msg_len];

        let n = BigUint::from_bytes_be(n_bytes);
        let e = BigUint::from_bytes_be(e_bytes);

        let rsa_pub = match RsaPublicKey::new(n, e) {
            Ok(k) => k,
            Err(_) => return vec![STATUS_ERR],
        };

        let signature = match RsaSignature::try_from(sig_bytes) {
            Ok(s) => s,
            Err(_) => return vec![STATUS_ERR],
        };

        // Verify with PKCS#1 v1.5 using the appropriate hash
        match hash_id {
            0 => { // SHA256
                let vk = RsaVerifyingKey::<Sha256>::new(rsa_pub);
                match vk.verify(msg, &signature) {
                    Ok(()) => vec![STATUS_OK],
                    Err(_) => vec![STATUS_ERR],
                }
            }
            1 => { // SHA384
                let vk = RsaVerifyingKey::<Sha384>::new(rsa_pub);
                match vk.verify(msg, &signature) {
                    Ok(()) => vec![STATUS_OK],
                    Err(_) => vec![STATUS_ERR],
                }
            }
            2 => { // SHA512
                let vk = RsaVerifyingKey::<Sha512>::new(rsa_pub);
                match vk.verify(msg, &signature) {
                    Ok(()) => vec![STATUS_OK],
                    Err(_) => vec![STATUS_ERR],
                }
            }
            3 => { // SHA1
                let vk = RsaVerifyingKey::<Sha1>::new(rsa_pub);
                match vk.verify(msg, &signature) {
                    Ok(()) => vec![STATUS_OK],
                    Err(_) => vec![STATUS_ERR],
                }
            }
            _ => { // Default SHA256
                let vk = RsaVerifyingKey::<Sha256>::new(rsa_pub);
                match vk.verify(msg, &signature) {
                    Ok(()) => vec![STATUS_OK],
                    Err(_) => vec![STATUS_ERR],
                }
            }
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

    fn aes_gcm_encrypt(&self, data: &[u8]) -> Vec<u8> {
        // Wire: [key_len:1][iv_len:1][aad_len:u16le][pt_len:u32le][key][iv][aad][plaintext]
        // Response: [STATUS_OK][ciphertext][tag:16]
        if data.len() < 8 {
            return vec![STATUS_ERR];
        }

        let key_len = data[0] as usize;
        let iv_len = data[1] as usize;
        let aad_len = u16::from_le_bytes(data[2..4].try_into().unwrap()) as usize;
        let pt_len = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        let header = 8;

        if data.len() < header + key_len + iv_len + aad_len + pt_len {
            return vec![STATUS_ERR];
        }

        let key = &data[header..header + key_len];
        let iv = &data[header + key_len..header + key_len + iv_len];
        let aad = &data[header + key_len + iv_len..header + key_len + iv_len + aad_len];
        let plaintext = &data[header + key_len + iv_len + aad_len..header + key_len + iv_len + aad_len + pt_len];

        let nonce = Nonce::from_slice(iv);

        let result = match key_len {
            16 => {
                let cipher = Aes128Gcm::new_from_slice(key).unwrap();
                cipher.encrypt(nonce, aes_gcm::aead::Payload { msg: plaintext, aad })
            }
            32 => {
                let cipher = Aes256Gcm::new_from_slice(key).unwrap();
                cipher.encrypt(nonce, aes_gcm::aead::Payload { msg: plaintext, aad })
            }
            _ => return vec![STATUS_ERR],
        };

        match result {
            Ok(ciphertext_and_tag) => {
                let mut out = Vec::with_capacity(1 + ciphertext_and_tag.len());
                out.push(STATUS_OK);
                out.extend_from_slice(&ciphertext_and_tag);
                out
            }
            Err(_) => vec![STATUS_ERR],
        }
    }

    fn aes_gcm_decrypt(&self, data: &[u8]) -> Vec<u8> {
        // Wire: [key_len:1][iv_len:1][aad_len:u16le][ct_len:u32le][key][iv][aad][ciphertext+tag]
        // Response: [STATUS_OK][plaintext]
        if data.len() < 8 {
            return vec![STATUS_ERR];
        }

        let key_len = data[0] as usize;
        let iv_len = data[1] as usize;
        let aad_len = u16::from_le_bytes(data[2..4].try_into().unwrap()) as usize;
        let ct_len = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        let header = 8;

        eprintln!("[aes-gcm-dec] key_len={} iv_len={} aad_len={} ct_len={} data.len={} expected={}",
            key_len, iv_len, aad_len, ct_len, data.len(), header + key_len + iv_len + aad_len + ct_len);

        if data.len() < header + key_len + iv_len + aad_len + ct_len {
            return vec![STATUS_ERR];
        }

        let key = &data[header..header + key_len];
        let iv = &data[header + key_len..header + key_len + iv_len];
        let aad = &data[header + key_len + iv_len..header + key_len + iv_len + aad_len];
        let ciphertext_and_tag = &data[header + key_len + iv_len + aad_len..header + key_len + iv_len + aad_len + ct_len];

        let nonce = Nonce::from_slice(iv);

        let result = match key_len {
            16 => {
                let cipher = Aes128Gcm::new_from_slice(key).unwrap();
                cipher.decrypt(nonce, aes_gcm::aead::Payload { msg: ciphertext_and_tag, aad })
            }
            32 => {
                let cipher = Aes256Gcm::new_from_slice(key).unwrap();
                cipher.decrypt(nonce, aes_gcm::aead::Payload { msg: ciphertext_and_tag, aad })
            }
            _ => return vec![STATUS_ERR],
        };

        match result {
            Ok(plaintext) => {
                let mut out = Vec::with_capacity(1 + plaintext.len());
                out.push(STATUS_OK);
                out.extend_from_slice(&plaintext);
                out
            }
            Err(_) => vec![STATUS_ERR],
        }
    }
}
