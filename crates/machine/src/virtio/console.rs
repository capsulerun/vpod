use super::{RamView, VirtioMmio, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE};

const DEVICE_ID: u32 = 3;
const VIRTIO_F_VERSION_1: u64 = 1u64 << 32;
const DEVICE_FEATURES: u64 = (1 << 0) | VIRTIO_F_VERSION_1;

const QUEUE_RX: usize = 0;
const QUEUE_TX: usize = 1;

pub struct VirtioConsole {
    pub mmio: VirtioMmio,
    rx_pending: std::collections::VecDeque<u8>,
    pub tx_buf: Vec<u8>,
}

impl VirtioConsole {
    pub fn new() -> Self {
        let mut mmio = VirtioMmio::new(DEVICE_ID, DEVICE_FEATURES, 2);
        mmio.config[0..2].copy_from_slice(&80u16.to_le_bytes());
        mmio.config[2..4].copy_from_slice(&24u16.to_le_bytes());
        Self {
            mmio,
            rx_pending: std::collections::VecDeque::new(),
            tx_buf: Vec::new()
        }
    }

    pub fn push_rx(&mut self, byte: u8) {
        self.rx_pending.push_back(byte);
    }

    pub fn notify(&mut self, queue_idx: usize, ram: &mut RamView) {
        match queue_idx {
            QUEUE_TX => self.drain_tx(ram),
            QUEUE_RX => self.flush_rx(ram),
            _ => {}
        }
    }

    pub fn flush_rx(&mut self, ram: &mut RamView) {
        if self.rx_pending.is_empty() {
            return;
        }

        loop {
            let Some(head) = self.mmio.queues[QUEUE_RX].pop_avail(ram) else {
                break;
            };
            let mut desc = self.mmio.queues[QUEUE_RX].read_desc(ram, head);
            let mut total = 0u32;

            loop {
                if desc.flags & VRING_DESC_F_WRITE != 0 {
                    let cap = desc.len as usize;
                    let n = cap.min(self.rx_pending.len());
                    for j in 0..n {
                        ram.write_u8(desc.addr + j as u64, self.rx_pending.pop_front().unwrap());
                    }
                    total += n as u32;
                }
                if desc.flags & VRING_DESC_F_NEXT == 0 {
                    break;
                }
                desc = self.mmio.queues[QUEUE_RX].read_desc(ram, desc.next);
            }

            self.mmio.queues[QUEUE_RX].push_used(ram, head, total);
            self.mmio.int_status |= 1;

            if self.rx_pending.is_empty() {
                break;
            }
        }
    }

    fn drain_tx(&mut self, ram: &mut RamView) {
        loop {
            let Some(head) = self.mmio.queues[QUEUE_TX].pop_avail(ram) else {
                break;
            };
            let mut desc = self.mmio.queues[QUEUE_TX].read_desc(ram, head);

            loop {
                if desc.flags & VRING_DESC_F_WRITE == 0 {
                    let mut buf = vec![0u8; desc.len as usize];
                    ram.read_bytes(desc.addr, &mut buf);
                    self.tx_buf.extend_from_slice(&buf);
                }
                if desc.flags & VRING_DESC_F_NEXT == 0 {
                    break;
                }
                desc = self.mmio.queues[QUEUE_TX].read_desc(ram, desc.next);
            }

            self.mmio.queues[QUEUE_TX].push_used(ram, head, 0);
            self.mmio.int_status |= 1;
        }
    }

    pub fn flush_tx_to_stdout(&mut self) {
        use std::io::Write;
        if !self.tx_buf.is_empty() {
            let _ = std::io::stdout().write_all(&self.tx_buf);
            let _ = std::io::stdout().flush();
            self.tx_buf.clear();
        }
    }
}

impl Default for VirtioConsole {
    fn default() -> Self {
        Self::new()
    }
}
