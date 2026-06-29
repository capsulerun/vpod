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

const VIRTIO_CRYPTO_NOTSUPP: u8 = 3;

const NUM_QUEUES: usize = 2;
const DATAQ: usize = 0;
const CONTROLQ: usize = 1;

pub struct VirtioCrypto {
    pub mmio: VirtioMmio,
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
        };

        let max_dataqueues: u32 = 1;
        let crypto_services: u32 =
            (1 << VIRTIO_CRYPTO_SERVICE_AEAD) | (1 << VIRTIO_CRYPTO_SERVICE_AKCIPHER);

        device.mmio.config[0..4].copy_from_slice(&0u32.to_le_bytes()); // status = OK
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
            let d = self.mmio.queues[DATAQ].read_desc(ram, idx);
            if d.flags & VRING_DESC_F_WRITE != 0 {
                last_write_addr = d.addr;
                last_write_len = d.len;
            }

            if d.flags & VRING_DESC_F_NEXT != 0 {
                idx = d.next;
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
        let mut idx = head;
        let mut last_write_addr = 0u64;
        let mut last_write_len = 0u32;

        loop {
            let d = self.mmio.queues[CONTROLQ].read_desc(ram, idx);
            if d.flags & VRING_DESC_F_WRITE != 0 {
                last_write_addr = d.addr;
                last_write_len = d.len;
            }

            if d.flags & VRING_DESC_F_NEXT != 0 {
                idx = d.next;
            } else {
                break;
            }
        }

        if last_write_addr != 0 {
            ram.write_u8(last_write_addr, VIRTIO_CRYPTO_NOTSUPP);
        }

        last_write_len
    }
}
