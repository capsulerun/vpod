use super::{RamView, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE, VirtioMmio};
use std::collections::VecDeque;

const DEVICE_ID: u32 = 1;
const VIRTIO_F_VERSION_1: u64 = 1u64 << 32;
const DEVICE_FEATURES: u64 = (1 << 5) | VIRTIO_F_VERSION_1;

const QUEUE_RX: usize = 0;
const QUEUE_TX: usize = 1;

const VNET_HDR_LEN: usize = 12;

pub trait NetworkBackend {
    fn send(&mut self, frame: &[u8]);
    fn recv(&mut self) -> Option<Vec<u8>>;
    fn has_rx(&self) -> bool;
    fn has_active_connections(&self) -> bool;
}

pub struct VirtioNet<B: NetworkBackend> {
    pub mmio: VirtioMmio,
    backend: B,
    rx_hold: VecDeque<Vec<u8>>,
}

impl<B: NetworkBackend> VirtioNet<B> {
    pub fn new(backend: B, mac: [u8; 6]) -> Self {
        let mut mmio = VirtioMmio::new(DEVICE_ID, DEVICE_FEATURES, 2);

        mmio.config[0..6].copy_from_slice(&mac);
        mmio.config[6..8].copy_from_slice(&1u16.to_le_bytes());

        Self {
            mmio,
            backend,
            rx_hold: VecDeque::new(),
        }
    }

    pub fn rx_pending(&self) -> bool {
        !self.rx_hold.is_empty() || self.backend.has_rx()
    }

    pub fn has_active_connections(&self) -> bool {
        self.backend.has_active_connections()
    }

    pub fn notify(&mut self, queue_idx: usize, ram: &mut RamView) {
        if queue_idx == QUEUE_TX {
            self.drain_tx(ram);
        }
    }

    pub fn poll_rx(&mut self, ram: &mut RamView) {
        while let Some(frame) = self.rx_hold.pop_front() {
            if !self.push_rx_frame(ram, &frame) {
                self.rx_hold.push_front(frame);
                return;
            }
        }
        while let Some(frame) = self.backend.recv() {
            if !self.push_rx_frame(ram, &frame) {
                self.rx_hold.push_back(frame);
                return;
            }
        }
    }

    fn push_rx_frame(&mut self, ram: &mut RamView, frame: &[u8]) -> bool {
        let Some(head) = self.mmio.queues[QUEUE_RX].pop_avail(ram) else {
            return false;
        };
        let mut desc = self.mmio.queues[QUEUE_RX].read_desc(ram, head);
        let total_len = VNET_HDR_LEN + frame.len();
        let mut written = 0usize;

        loop {
            if desc.flags & VRING_DESC_F_WRITE != 0 {
                let cap = desc.len as usize;
                let mut buf_off = 0usize;

                if written < VNET_HDR_LEN {
                    let hdr_bytes = (VNET_HDR_LEN - written).min(cap);
                    for j in 0..hdr_bytes {
                        ram.write_u8(desc.addr + j as u64, 0);
                    }
                    written += hdr_bytes;
                    buf_off += hdr_bytes;
                }

                if written >= VNET_HDR_LEN && buf_off < cap {
                    let frame_off = written - VNET_HDR_LEN;
                    let n = (cap - buf_off).min(frame.len().saturating_sub(frame_off));
                    if n > 0 {
                        ram.write_bytes(
                            desc.addr + buf_off as u64,
                            &frame[frame_off..frame_off + n],
                        );
                        written += n;
                    }
                }
            }

            if desc.flags & VRING_DESC_F_NEXT == 0 || written >= total_len {
                break;
            }

            desc = self.mmio.queues[QUEUE_RX].read_desc(ram, desc.next);
        }

        self.mmio.queues[QUEUE_RX].push_used(ram, head, written as u32);
        self.mmio.int_status |= 1;
        true
    }

    fn drain_tx(&mut self, ram: &mut RamView) {
        while let Some(head) = self.mmio.queues[QUEUE_TX].pop_avail(ram) {
            let mut frame: Vec<u8> = Vec::new();
            let mut desc = self.mmio.queues[QUEUE_TX].read_desc(ram, head);
            let mut hdr_skipped = 0usize;

            loop {
                if desc.flags & VRING_DESC_F_WRITE == 0 {
                    let len = desc.len as usize;
                    if hdr_skipped < VNET_HDR_LEN {
                        let skip = (VNET_HDR_LEN - hdr_skipped).min(len);
                        hdr_skipped += skip;
                        let rem = len - skip;
                        if rem > 0 {
                            let start = desc.addr + skip as u64;
                            let mut buf = vec![0u8; rem];
                            ram.read_bytes(start, &mut buf);
                            frame.extend_from_slice(&buf);
                        }
                    } else {
                        let mut buf = vec![0u8; len];
                        ram.read_bytes(desc.addr, &mut buf);
                        frame.extend_from_slice(&buf);
                    }
                }
                if desc.flags & VRING_DESC_F_NEXT == 0 {
                    break;
                }
                desc = self.mmio.queues[QUEUE_TX].read_desc(ram, desc.next);
            }

            if !frame.is_empty() {
                self.backend.send(&frame);
            }
            self.mmio.queues[QUEUE_TX].push_used(ram, head, 0);
            self.mmio.int_status |= 1;
        }
    }
}
