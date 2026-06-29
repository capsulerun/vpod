
use std::collections::HashMap;

use super::{RamView, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE, VirtioMmio};

const DEVICE_ID: u32 = 20; // VIRTIO_DEVICE_ID_CRYPTO
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

const VIRTIO_CRYPTO_CREATE_SESSION: u32 = 0;
const VIRTIO_CRYPTO_DESTROY_SESSION: u32 = 1;

const VIRTIO_CRYPTO_AEAD_AES_GCM: u32 = 1;
const VIRTIO_CRYPTO_AEAD_CHACHA20_POLY1305: u32 = 3;

const VIRTIO_CRYPTO_AKCIPHER_RSA: u32 = 1;
const VIRTIO_CRYPTO_AKCIPHER_ECDSA: u32 = 2;

// const VIRTIO_CRYPTO_AKCIPHER_KEY_TYPE_PUBLIC: u32 = 1;
// const VIRTIO_CRYPTO_AKCIPHER_KEY_TYPE_PRIVATE: u32 = 2;

const NUM_QUEUES: usize = 2;
const DATAQ: usize = 0;
const CONTROLQ: usize = 1;

#[derive(Clone, Debug)]
pub enum AeadAlgorithm {
    AesGcm,
    ChaCha20Poly1305,
}

#[derive(Clone, Debug)]
pub enum AkCipherAlgorithm {
    Rsa,
    Ecdsa,
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

        device.mmio.config[0..4].copy_from_slice(&0u32.to_le_bytes());
        device.mmio.config[4..8].copy_from_slice(&max_dataqueues.to_le_bytes());
        device.mmio.config[8..12].copy_from_slice(&crypto_services.to_le_bytes());

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
        let mut idx = head;
        let mut last_write_addr = 0u64;
        let mut last_write_len = 0u32;

        loop {
            let descriptor = self.mmio.queues[DATAQ].read_desc(ram, idx);
            if descriptor.flags & VRING_DESC_F_WRITE != 0 {
                last_write_addr = descriptor.addr;
                last_write_len = descriptor.len;
            }

            if descriptor.flags & VRING_DESC_F_NEXT != 0 {
                idx = descriptor.next;
            } else {
                break;
            }
        }

        if last_write_addr != 0 {
            ram.write_u8(last_write_addr, VIRTIO_CRYPTO_NOTSUPP);
        }

        last_write_len
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

        let (status, session_id) = match opcode {
            VIRTIO_CRYPTO_CREATE_SESSION => self.create_session(ram, &read_bufs),
            VIRTIO_CRYPTO_DESTROY_SESSION => self.destroy_session(ram, &read_bufs),
            _ => (VIRTIO_CRYPTO_NOTSUPP, 0u64),
        };

        let (resp_addr, resp_len) = write_bufs[0];
        if opcode == VIRTIO_CRYPTO_CREATE_SESSION && resp_len >= 9 {
            ram.write_u64(resp_addr, session_id);
            ram.write_u8(resp_addr + 8, status);
            9
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

        if header_len < 8 {
            return (VIRTIO_CRYPTO_ERR, 0);
        }

        let opcode = ram.read_u32(header_addr);
        let _ = opcode;

        if header_len < 16 {
            return (VIRTIO_CRYPTO_ERR, 0);
        }

        let algo = ram.read_u32(header_addr + 4);
        let service = ram.read_u32(header_addr + 8);

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
            VIRTIO_CRYPTO_AEAD_AES_GCM => AeadAlgorithm::AesGcm,
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

        self.sessions.insert(
            session_id,
            CryptoSession::Aead { algorithm, key },
        );

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
            _ => return (VIRTIO_CRYPTO_NOTSUPP, 0),
        };

        let header_addr = read_bufs[0].0;
        let header_len = read_bufs[0].1;

        if header_len < 24 {
            return (VIRTIO_CRYPTO_ERR, 0);
        }

        let key_type = ram.read_u32(header_addr + 12);
        let key_len = ram.read_u32(header_addr + 16) as usize;
        let key = self.read_key_data(ram, read_bufs, 20, key_len);

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

        if header_len < 12 {
            return (VIRTIO_CRYPTO_ERR, 0);
        }

        let session_id = ram.read_u64(header_addr + 4);

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
        if key_len == 0 {
            return Vec::new();
        }

        let mut key = vec![0u8; key_len];
        let mut remaining = key_len;
        let mut dst_offset = 0;

        let first_addr = read_bufs[0].0;
        let first_len = read_bufs[0].1 as usize;
        let available_in_first = first_len.saturating_sub(offset_in_first);
        let to_read = remaining.min(available_in_first);

        if to_read > 0 {
            ram.read_bytes(
                first_addr + offset_in_first as u64,
                &mut key[..to_read],
            );
            remaining -= to_read;
            dst_offset += to_read;
        }

        for &(addr, len) in &read_bufs[1..] {
            if remaining == 0 {
                break;
            }
            let to_read = remaining.min(len as usize);
            ram.read_bytes(addr, &mut key[dst_offset..dst_offset + to_read]);
            remaining -= to_read;
            dst_offset += to_read;
        }

        key
    }

    pub fn get_session(&self, session_id: u64) -> Option<&CryptoSession> {
        self.sessions.get(&session_id)
    }
}
