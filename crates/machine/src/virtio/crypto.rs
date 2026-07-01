use std::collections::HashMap;

use aes_gcm::{Aes128Gcm, Aes256Gcm, KeyInit};
use chacha20poly1305::ChaCha20Poly1305;
use p256::ecdh::EphemeralSecret;
use p256::ecdsa::{SigningKey, VerifyingKey, signature::Signer, signature::Verifier};
use p256::{EncodedPoint, PublicKey};
use p256::elliptic_curve::rand_core::OsRng;
use rsa::pkcs1v15::{SigningKey as RsaSigningKey, VerifyingKey as RsaVerifyingKey};
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
use rsa::signature::SignatureEncoding;
use rsa::{RsaPrivateKey, RsaPublicKey};
use sha2::Sha256;

use super::{RamView, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE, VirtioMmio};

const DEVICE_ID: u32 = 20;
const VIRTIO_F_VERSION_1: u64 = 1u64 << 32;

const VIRTIO_CRYPTO_SERVICE_AEAD: u32 = 3;
const VIRTIO_CRYPTO_SERVICE_AKCIPHER: u32 = 4;

const VIRTIO_CRYPTO_F_REVISION_1: u64 = 1 << 0;
const VIRTIO_CRYPTO_F_AEAD_STATELESS: u64 = 1 << 4;
const VIRTIO_CRYPTO_F_AKCIPHER_STATELESS: u64 = 1 << 5;

const DEVICE_FEATURES: u64 = VIRTIO_F_VERSION_1
    | VIRTIO_CRYPTO_F_REVISION_1
    | VIRTIO_CRYPTO_F_AEAD_STATELESS
    | VIRTIO_CRYPTO_F_AKCIPHER_STATELESS;

const VIRTIO_CRYPTO_OK: u8 = 0;
const VIRTIO_CRYPTO_ERR: u8 = 1;
const VIRTIO_CRYPTO_NOTSUPP: u8 = 3;
const VIRTIO_CRYPTO_INVSESS: u8 = 4;

const VIRTIO_CRYPTO_AKCIPHER_CREATE_SESSION: u32 = (VIRTIO_CRYPTO_SERVICE_AKCIPHER << 8) | 0x04;
const VIRTIO_CRYPTO_AKCIPHER_DESTROY_SESSION: u32 = (VIRTIO_CRYPTO_SERVICE_AKCIPHER << 8) | 0x05;
const VIRTIO_CRYPTO_AEAD_CREATE_SESSION: u32 = (VIRTIO_CRYPTO_SERVICE_AEAD << 8) | 0x02;
const VIRTIO_CRYPTO_AEAD_DESTROY_SESSION: u32 = (VIRTIO_CRYPTO_SERVICE_AEAD << 8) | 0x03;

const VIRTIO_CRYPTO_AEAD_ENCRYPT: u32 = 0x0400;
const VIRTIO_CRYPTO_AEAD_DECRYPT: u32 = 0x0401;
const VIRTIO_CRYPTO_AKCIPHER_ENCRYPT: u32 = 0x0600;
const VIRTIO_CRYPTO_AKCIPHER_DECRYPT: u32 = 0x0601;
const VIRTIO_CRYPTO_AKCIPHER_SIGN: u32 = 0x0602;
const VIRTIO_CRYPTO_AKCIPHER_VERIFY: u32 = 0x0603;

const VIRTIO_CRYPTO_AEAD_AES_128_GCM: u32 = 1;
const VIRTIO_CRYPTO_AEAD_AES_256_GCM: u32 = 2;
const VIRTIO_CRYPTO_AEAD_CHACHA20_POLY1305: u32 = 3;

const VIRTIO_CRYPTO_AKCIPHER_RSA: u32 = 1;
const VIRTIO_CRYPTO_AKCIPHER_ECDSA: u32 = 2;
const VIRTIO_CRYPTO_AKCIPHER_ECDH: u32 = 3;

const NUM_QUEUES: usize = 2;
const DATAQ: usize = 0;
const CONTROLQ: usize = 1;

#[derive(Clone, Debug)]
pub enum AeadAlgorithm {
    Aes128Gcm,
    Aes256Gcm,
    ChaCha20Poly1305,
}

#[derive(Clone, Debug)]
pub enum AkCipherAlgorithm {
    Rsa,
    Ecdsa,
    Ecdh,
}

#[derive(Clone, Debug)]
pub enum CryptoSession {
    Aead {
        algorithm: AeadAlgorithm,
        key: Vec<u8>,
    },
    AkCipher {
        algorithm: AkCipherAlgorithm,
        key: Vec<u8>,
        key_type: u32,
    },
}

pub struct VirtioCrypto {
    pub mmio: VirtioMmio,
    sessions: HashMap<u64, CryptoSession>,
    next_session_id: u64,
}

impl Default for VirtioCrypto {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtioCrypto {
    pub fn new() -> Self {
        let mut device = Self {
            mmio: VirtioMmio::new(DEVICE_ID, DEVICE_FEATURES, NUM_QUEUES),
            sessions: HashMap::new(),
            next_session_id: 1,
        };

        let max_dataqueues: u32 = 1;
        let crypto_services: u32 =
            (1 << VIRTIO_CRYPTO_SERVICE_AEAD) | (1 << VIRTIO_CRYPTO_SERVICE_AKCIPHER);


        device.mmio.config[0..4].copy_from_slice(&1u32.to_le_bytes());
        device.mmio.config[4..8].copy_from_slice(&max_dataqueues.to_le_bytes());
        device.mmio.config[8..12].copy_from_slice(&crypto_services.to_le_bytes());
        device.mmio.config[12..16].copy_from_slice(&0u32.to_le_bytes());
        device.mmio.config[16..20].copy_from_slice(&0u32.to_le_bytes());
        device.mmio.config[20..24].copy_from_slice(&(1u32 << 2).to_le_bytes());
        device.mmio.config[24..28].copy_from_slice(&0u32.to_le_bytes());
        device.mmio.config[28..32].copy_from_slice(&0u32.to_le_bytes());
        device.mmio.config[32..36].copy_from_slice(&0x7u32.to_le_bytes());
        device.mmio.config[36..40].copy_from_slice(&64u32.to_le_bytes());
        device.mmio.config[40..44].copy_from_slice(&64u32.to_le_bytes());
        device.mmio.config[44..48].copy_from_slice(&0x3u32.to_le_bytes());
        device.mmio.config[48..56].copy_from_slice(&(16u64 * 1024 * 1024).to_le_bytes());

        device
    }

    pub fn notify(&mut self, queue_index: usize, ram: &mut RamView) {
        match queue_index {
            DATAQ => self.process_dataq(ram),
            CONTROLQ => self.process_controlq(ram),
            _ => {}
        }
    }

    fn process_dataq(&mut self, ram: &mut RamView) {
        while let Some(head) = self.mmio.queues[DATAQ].pop_avail(ram) {
            let used_len = self.handle_data_request(ram, head);
            self.mmio.queues[DATAQ].push_used(ram, head, used_len);
            self.mmio.int_status |= 1;
        }
    }

    fn process_controlq(&mut self, ram: &mut RamView) {
        while let Some(head) = self.mmio.queues[CONTROLQ].pop_avail(ram) {
            let used_len = self.handle_control_request(ram, head);
            self.mmio.queues[CONTROLQ].push_used(ram, head, used_len);
            self.mmio.int_status |= 1;
        }
    }

    fn handle_data_request(&mut self, ram: &mut RamView, head: u16) -> u32 {
        let mut read_bufs: Vec<(u64, u32)> = Vec::new();
        let mut write_bufs: Vec<(u64, u32)> = Vec::new();
        let mut idx = head;

        loop {
            let descriptor = self.mmio.queues[DATAQ].read_desc(ram, idx);
            if descriptor.flags & VRING_DESC_F_WRITE != 0 {
                write_bufs.push((descriptor.addr, descriptor.len));
            } else {
                read_bufs.push((descriptor.addr, descriptor.len));
            }

            if descriptor.flags & VRING_DESC_F_NEXT != 0 {
                idx = descriptor.next;
            } else {
                break;
            }
        }

        if read_bufs.is_empty() || write_bufs.is_empty() {
            return 0;
        }

        let header_addr = read_bufs[0].0;
        let header_len = read_bufs[0].1;

        if header_len < 16 {
            return self.write_status_to_last_write(ram, &write_bufs, VIRTIO_CRYPTO_ERR);
        }

        let opcode = ram.read_u32(header_addr);
        let session_id = ram.read_u64(header_addr + 8);

        match opcode {
            VIRTIO_CRYPTO_AEAD_ENCRYPT | VIRTIO_CRYPTO_AEAD_DECRYPT => {
                self.handle_aead_op(ram, &read_bufs, &write_bufs, opcode, session_id)
            }
            VIRTIO_CRYPTO_AKCIPHER_SIGN => {
                self.handle_akcipher_sign(ram, &read_bufs, &write_bufs, session_id)
            }
            VIRTIO_CRYPTO_AKCIPHER_VERIFY => {
                self.handle_akcipher_verify(ram, &read_bufs, &write_bufs, session_id)
            }
            VIRTIO_CRYPTO_AKCIPHER_ENCRYPT | VIRTIO_CRYPTO_AKCIPHER_DECRYPT => {
                self.handle_akcipher_op(ram, &read_bufs, &write_bufs, opcode, session_id)
            }
            _ => self.write_status_to_last_write(ram, &write_bufs, VIRTIO_CRYPTO_NOTSUPP),
        }
    }

    fn handle_aead_op(
        &self,
        ram: &mut RamView,
        read_bufs: &[(u64, u32)],
        write_bufs: &[(u64, u32)],
        opcode: u32,
        session_id: u64,
    ) -> u32 {
        let session = match self.sessions.get(&session_id) {
            Some(s) => s,
            None => return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_INVSESS),
        };

        let (algorithm, key) = match session {
            CryptoSession::Aead { algorithm, key } => (algorithm, key),
            _ => return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR),
        };

        let header_addr = read_bufs[0].0;
        let header_len = read_bufs[0].1;

        if header_len < 28 {
            return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR);
        }

        let iv_len = ram.read_u32(header_addr + 16) as usize;
        let aad_len = ram.read_u32(header_addr + 20) as usize;
        let src_data_len = ram.read_u32(header_addr + 24) as usize;

        let payload_offset = 28;
        let total_payload = iv_len + aad_len + src_data_len;
        let payload = self.read_data_from_chain(ram, read_bufs, payload_offset, total_payload);

        if payload.len() < total_payload {
            return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR);
        }

        let iv = &payload[..iv_len];
        let aad = &payload[iv_len..iv_len + aad_len];
        let src_data = &payload[iv_len + aad_len..];

        let nonce = aes_gcm::Nonce::from_slice(iv);

        let result_data = match algorithm {
            AeadAlgorithm::Aes128Gcm => {
                let cipher = match Aes128Gcm::new_from_slice(key) {
                    Ok(c) => c,
                    Err(_) => {
                        return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR)
                    }
                };
                self.do_aead(&cipher, nonce, aad, src_data, opcode)
            }
            AeadAlgorithm::Aes256Gcm => {
                let cipher = match Aes256Gcm::new_from_slice(key) {
                    Ok(c) => c,
                    Err(_) => {
                        return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR)
                    }
                };
                self.do_aead(&cipher, nonce, aad, src_data, opcode)
            }
            AeadAlgorithm::ChaCha20Poly1305 => {
                let cipher = match ChaCha20Poly1305::new_from_slice(key) {
                    Ok(c) => c,
                    Err(_) => {
                        return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR)
                    }
                };
                self.do_aead(&cipher, chacha20poly1305::Nonce::from_slice(iv), aad, src_data, opcode)
            }
        };

        match result_data {
            Some(data) => self.write_result_with_status(ram, write_bufs, &data, VIRTIO_CRYPTO_OK),
            None => self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR),
        }
    }

    fn do_aead<C: aes_gcm::aead::Aead>(
        &self,
        cipher: &C,
        nonce: &aes_gcm::aead::generic_array::GenericArray<u8, C::NonceSize>,
        aad: &[u8],
        src_data: &[u8],
        opcode: u32,
    ) -> Option<Vec<u8>>
    where
        C::NonceSize: aes_gcm::aead::generic_array::ArrayLength<u8>,
    {
        use aes_gcm::aead::Payload;
        let payload = Payload { msg: src_data, aad };
        if opcode == VIRTIO_CRYPTO_AEAD_ENCRYPT {
            cipher.encrypt(nonce, payload).ok()
        } else {
            cipher.decrypt(nonce, payload).ok()
        }
    }

    fn handle_akcipher_sign(
        &self,
        ram: &mut RamView,
        read_bufs: &[(u64, u32)],
        write_bufs: &[(u64, u32)],
        session_id: u64,
    ) -> u32 {
        let session = match self.sessions.get(&session_id) {
            Some(s) => s,
            None => return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_INVSESS),
        };

        let (algorithm, key) = match session {
            CryptoSession::AkCipher { algorithm, key, .. } => (algorithm, key),
            _ => return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR),
        };

        let header_addr = read_bufs[0].0;
        let header_len = read_bufs[0].1;

        if header_len < 20 {
            return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR);
        }

        let src_len = ram.read_u32(header_addr + 16) as usize;
        let src_data = self.read_data_from_chain(ram, read_bufs, 20, src_len);

        let sig = match algorithm {
            AkCipherAlgorithm::Rsa => {
                let private_key = match RsaPrivateKey::from_pkcs8_der(key) {
                    Ok(k) => k,
                    Err(_) => {
                        return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR);
                    }
                };
                let signing_key = RsaSigningKey::<Sha256>::new(private_key);
                match rsa::signature::Signer::sign(&signing_key, &src_data) {
                    sig => sig.to_vec(),
                }
            }
            AkCipherAlgorithm::Ecdsa => {
                let signing_key = match SigningKey::from_slice(key) {
                    Ok(k) => k,
                    Err(_) => {
                        return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR);
                    }
                };
                let sig: p256::ecdsa::DerSignature = signing_key.sign(&src_data);
                sig.to_vec()
            }
            AkCipherAlgorithm::Ecdh => {
                return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_NOTSUPP);
            }
        };

        self.write_result_with_status(ram, write_bufs, &sig, VIRTIO_CRYPTO_OK)
    }

    fn handle_akcipher_verify(
        &self,
        ram: &mut RamView,
        read_bufs: &[(u64, u32)],
        write_bufs: &[(u64, u32)],
        session_id: u64,
    ) -> u32 {
        let session = match self.sessions.get(&session_id) {
            Some(s) => s,
            None => return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_INVSESS),
        };

        let (algorithm, key) = match session {
            CryptoSession::AkCipher { algorithm, key, .. } => (algorithm, key),
            _ => return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR),
        };

        let header_addr = read_bufs[0].0;
        let header_len = read_bufs[0].1;

        if header_len < 24 {
            return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR);
        }

        let src_len = ram.read_u32(header_addr + 16) as usize;
        let sig_len = ram.read_u32(header_addr + 20) as usize;
        let payload = self.read_data_from_chain(ram, read_bufs, 24, src_len + sig_len);

        if payload.len() < src_len + sig_len {
            return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR);
        }

        let src_data = &payload[..src_len];
        let sig_data = &payload[src_len..src_len + sig_len];

        let status = match algorithm {
            AkCipherAlgorithm::Rsa => {
                let public_key = match RsaPublicKey::from_public_key_der(key) {
                    Ok(k) => k,
                    Err(_) => return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR),
                };
                let verifying_key = RsaVerifyingKey::<Sha256>::new(public_key);
                let signature = match rsa::pkcs1v15::Signature::try_from(sig_data) {
                    Ok(s) => s,
                    Err(_) => return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR),
                };
                match verifying_key.verify(src_data, &signature) {
                    Ok(()) => VIRTIO_CRYPTO_OK,
                    Err(_) => VIRTIO_CRYPTO_ERR,
                }
            }
            AkCipherAlgorithm::Ecdsa => {
                let verifying_key = match VerifyingKey::from_sec1_bytes(key) {
                    Ok(k) => k,
                    Err(_) => return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR),
                };
                let signature = match p256::ecdsa::DerSignature::try_from(sig_data) {
                    Ok(s) => s,
                    Err(_) => return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR),
                };
                match verifying_key.verify(src_data, &signature) {
                    Ok(()) => VIRTIO_CRYPTO_OK,
                    Err(_) => VIRTIO_CRYPTO_ERR,
                }
            }
            AkCipherAlgorithm::Ecdh => VIRTIO_CRYPTO_NOTSUPP,
        };

        self.write_status_to_last_write(ram, write_bufs, status)
    }

    fn handle_akcipher_op(
        &self,
        ram: &mut RamView,
        read_bufs: &[(u64, u32)],
        write_bufs: &[(u64, u32)],
        _opcode: u32,
        session_id: u64,
    ) -> u32 {
        let session = match self.sessions.get(&session_id) {
            Some(s) => s,
            None => return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_INVSESS),
        };

        let algorithm = match session {
            CryptoSession::AkCipher { algorithm, .. } => algorithm,
            _ => return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR),
        };

        match algorithm {
            AkCipherAlgorithm::Ecdh => {
                let header_addr = read_bufs[0].0;
                let header_len = read_bufs[0].1;

                if header_len < 20 {
                    return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR);
                }

                let src_len = ram.read_u32(header_addr + 16) as usize;
                let peer_public_key_bytes = self.read_data_from_chain(ram, read_bufs, 20, src_len);

                let secret = EphemeralSecret::random(&mut OsRng);
                let our_public_key = EncodedPoint::from(secret.public_key());
                let our_public_bytes = our_public_key.as_bytes();

                let peer_public_key = match PublicKey::from_sec1_bytes(&peer_public_key_bytes) {
                    Ok(k) => k,
                    Err(_) => {
                        return self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_ERR);
                    }
                };

                let shared_secret = secret.diffie_hellman(&peer_public_key);
                let shared_bytes = shared_secret.raw_secret_bytes();

                let mut result =
                    Vec::with_capacity(our_public_bytes.len() + shared_bytes.len() + 4);
                result.extend_from_slice(&(our_public_bytes.len() as u32).to_le_bytes());
                result.extend_from_slice(our_public_bytes);
                result.extend_from_slice(shared_bytes);

                self.write_result_with_status(ram, write_bufs, &result, VIRTIO_CRYPTO_OK)
            }
            _ => self.write_status_to_last_write(ram, write_bufs, VIRTIO_CRYPTO_NOTSUPP),
        }
    }

    fn handle_control_request(&mut self, ram: &mut RamView, head: u16) -> u32 {
        let mut read_bufs: Vec<(u64, u32)> = Vec::new();
        let mut write_bufs: Vec<(u64, u32)> = Vec::new();
        let mut idx = head;

        loop {
            let descriptor = self.mmio.queues[CONTROLQ].read_desc(ram, idx);
            if descriptor.flags & VRING_DESC_F_WRITE != 0 {
                write_bufs.push((descriptor.addr, descriptor.len));
            } else {
                read_bufs.push((descriptor.addr, descriptor.len));
            }

            if descriptor.flags & VRING_DESC_F_NEXT != 0 {
                idx = descriptor.next;
            } else {
                break;
            }
        }

        if read_bufs.is_empty() || write_bufs.is_empty() {
            return 0;
        }

        let header_addr = read_bufs[0].0;
        let opcode = ram.read_u32(header_addr);
        let is_create = opcode == VIRTIO_CRYPTO_AKCIPHER_CREATE_SESSION
            || opcode == VIRTIO_CRYPTO_AEAD_CREATE_SESSION;

        let (status, session_id) = match opcode {
            VIRTIO_CRYPTO_AKCIPHER_CREATE_SESSION | VIRTIO_CRYPTO_AEAD_CREATE_SESSION => {
                self.create_session(ram, &read_bufs)
            }
            VIRTIO_CRYPTO_AKCIPHER_DESTROY_SESSION | VIRTIO_CRYPTO_AEAD_DESTROY_SESSION => {
                self.destroy_session(ram, &read_bufs)
            }
            _ => (VIRTIO_CRYPTO_NOTSUPP, 0u64),
        };

        let (resp_addr, resp_len) = write_bufs[0];
        if is_create && resp_len >= 12 {
            ram.write_u64(resp_addr, session_id);
            ram.write_u32(resp_addr + 8, status as u32);
            12
        } else if resp_len >= 4 {
            ram.write_u32(resp_addr, status as u32);
            4
        } else {
            ram.write_u8(resp_addr, status);
            1
        }
    }

    fn create_session(&mut self, ram: &RamView, read_bufs: &[(u64, u32)]) -> (u8, u64) {
        if read_bufs.is_empty() {
            return (VIRTIO_CRYPTO_ERR, 0);
        }

        let header_addr = read_bufs[0].0;
        let header_len = read_bufs[0].1;

        if header_len < 16 {
            return (VIRTIO_CRYPTO_ERR, 0);
        }

        // ctrl_header: opcode(4) + algo(4) + flag(4) + queue_id(4) = 16 bytes
        let opcode = ram.read_u32(header_addr);
        let algo = ram.read_u32(header_addr + 4);
        let service = (opcode >> 8) & 0xFF;

        match service {
            VIRTIO_CRYPTO_SERVICE_AEAD => self.create_aead_session(ram, read_bufs, algo),
            VIRTIO_CRYPTO_SERVICE_AKCIPHER => self.create_akcipher_session(ram, read_bufs, algo),
            _ => (VIRTIO_CRYPTO_NOTSUPP, 0),
        }
    }

    fn create_aead_session(
        &mut self,
        ram: &RamView,
        read_bufs: &[(u64, u32)],
        algo: u32,
    ) -> (u8, u64) {
        let algorithm = match algo {
            VIRTIO_CRYPTO_AEAD_AES_128_GCM => AeadAlgorithm::Aes128Gcm,
            VIRTIO_CRYPTO_AEAD_AES_256_GCM => AeadAlgorithm::Aes256Gcm,
            VIRTIO_CRYPTO_AEAD_CHACHA20_POLY1305 => AeadAlgorithm::ChaCha20Poly1305,
            _ => return (VIRTIO_CRYPTO_NOTSUPP, 0),
        };

        let header_addr = read_bufs[0].0;
        let header_len = read_bufs[0].1;

        if header_len < 20 {
            return (VIRTIO_CRYPTO_ERR, 0);
        }

        let key_len = ram.read_u32(header_addr + 12) as usize;
        let key = self.read_key_data(ram, read_bufs, 16, key_len);

        let session_id = self.next_session_id;
        self.next_session_id += 1;

        self.sessions
            .insert(session_id, CryptoSession::Aead { algorithm, key });

        (VIRTIO_CRYPTO_OK, session_id)
    }

    fn create_akcipher_session(
        &mut self,
        ram: &RamView,
        read_bufs: &[(u64, u32)],
        algo: u32,
    ) -> (u8, u64) {
        let algorithm = match algo {
            VIRTIO_CRYPTO_AKCIPHER_RSA => AkCipherAlgorithm::Rsa,
            VIRTIO_CRYPTO_AKCIPHER_ECDSA => AkCipherAlgorithm::Ecdsa,
            VIRTIO_CRYPTO_AKCIPHER_ECDH => AkCipherAlgorithm::Ecdh,
            _ => return (VIRTIO_CRYPTO_NOTSUPP, 0),
        };

        let header_addr = read_bufs[0].0;
        let header_len = read_bufs[0].1;

        // ctrl_header(16) + akcipher_session_para starts at offset 16
        // para: algo(4) + keytype(4) + keylen(4) + rsa{padding_algo(4), hash_algo(4)}
        if header_len < 28 {
            return (VIRTIO_CRYPTO_ERR, 0);
        }

        let key_type = ram.read_u32(header_addr + 20);
        let key_len = ram.read_u32(header_addr + 24) as usize;

        // Key is in the second scatter-gather entry
        let key = if read_bufs.len() > 1 {
            let (key_addr, key_buf_len) = read_bufs[1];
            let len = key_len.min(key_buf_len as usize);
            let mut k = vec![0u8; len];
            for i in 0..len {
                k[i] = ram.read_u8(key_addr + i as u64);
            }
            k
        } else {
            Vec::new()
        };

        let session_id = self.next_session_id;
        self.next_session_id += 1;

        self.sessions.insert(
            session_id,
            CryptoSession::AkCipher {
                algorithm,
                key,
                key_type,
            },
        );

        (VIRTIO_CRYPTO_OK, session_id)
    }

    fn destroy_session(&mut self, ram: &RamView, read_bufs: &[(u64, u32)]) -> (u8, u64) {
        let header_addr = read_bufs[0].0;
        let header_len = read_bufs[0].1;

        if header_len < 24 {
            return (VIRTIO_CRYPTO_ERR, 0);
        }

        let session_id = ram.read_u64(header_addr + 16);

        if self.sessions.remove(&session_id).is_some() {
            (VIRTIO_CRYPTO_OK, 0)
        } else {
            (VIRTIO_CRYPTO_INVSESS, 0)
        }
    }

    fn read_key_data(
        &self,
        ram: &RamView,
        read_bufs: &[(u64, u32)],
        offset_in_first: usize,
        key_len: usize,
    ) -> Vec<u8> {
        self.read_data_from_chain(ram, read_bufs, offset_in_first, key_len)
    }

    fn read_data_from_chain(
        &self,
        ram: &RamView,
        read_bufs: &[(u64, u32)],
        offset_in_first: usize,
        total_len: usize,
    ) -> Vec<u8> {
        if total_len == 0 {
            return Vec::new();
        }

        let mut data = vec![0u8; total_len];
        let mut remaining = total_len;
        let mut dst_offset = 0;

        let first_addr = read_bufs[0].0;
        let first_len = read_bufs[0].1 as usize;
        let available_in_first = first_len.saturating_sub(offset_in_first);
        let to_read = remaining.min(available_in_first);

        if to_read > 0 {
            ram.read_bytes(first_addr + offset_in_first as u64, &mut data[..to_read]);
            remaining -= to_read;
            dst_offset += to_read;
        }

        for &(addr, len) in &read_bufs[1..] {
            if remaining == 0 {
                break;
            }
            let to_read = remaining.min(len as usize);
            ram.read_bytes(addr, &mut data[dst_offset..dst_offset + to_read]);
            remaining -= to_read;
            dst_offset += to_read;
        }

        data
    }

    fn write_status_to_last_write(
        &self,
        ram: &mut RamView,
        write_bufs: &[(u64, u32)],
        status: u8,
    ) -> u32 {
        if let Some(&(addr, len)) = write_bufs.last() {
            ram.write_u8(addr + len as u64 - 1, status);
            len
        } else {
            0
        }
    }

    fn write_result_with_status(
        &self,
        ram: &mut RamView,
        write_bufs: &[(u64, u32)],
        result_data: &[u8],
        status: u8,
    ) -> u32 {
        if write_bufs.is_empty() {
            return 0;
        }

        let mut total_written = 0u32;
        let mut data_remaining = result_data.len();
        let mut data_offset = 0;

        for (i, &(addr, len)) in write_bufs.iter().enumerate() {
            let is_last = i == write_bufs.len() - 1;

            if is_last {
                let space_for_data = (len as usize).saturating_sub(1);
                let to_write = data_remaining.min(space_for_data);
                if to_write > 0 {
                    ram.write_bytes(addr, &result_data[data_offset..data_offset + to_write]);
                }
                ram.write_u8(addr + to_write as u64, status);
                total_written += to_write as u32 + 1;
            } else {
                let to_write = data_remaining.min(len as usize);
                if to_write > 0 {
                    ram.write_bytes(addr, &result_data[data_offset..data_offset + to_write]);
                    data_remaining -= to_write;
                    data_offset += to_write;
                }
                total_written += to_write as u32;
            }
        }

        total_written
    }
}
