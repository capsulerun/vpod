// External communication linking the hart to RAM and peripherals (disk, network).
use std::sync::atomic::{AtomicU64, Ordering};

pub trait SystemBus {
    fn read_byte(&mut self, address: u64) -> u8;
    fn read_halfword(&mut self, address: u64) -> u16;
    fn read_word(&mut self, address: u64) -> u32;
    fn read_doubleword(&mut self, address: u64) -> u64;

    fn write_byte(&mut self, address: u64, val: u8);
    fn write_halfword(&mut self, address: u64, val: u16);
    fn write_word(&mut self, address: u64, val: u32);
    fn write_doubleword(&mut self, address: u64, val: u64);

    fn ram_load_page(&mut self, address: u64) -> Option<*const u8> {
        let _ = address;
        None
    }

    fn ram_store_page(&mut self, address: u64) -> Option<*mut u8> {
        let _ = address;
        None
    }

    fn ram_epoch(&self) -> u64 {
        0
    }
}

static FLAT_EPOCH_SOURCE: AtomicU64 = AtomicU64::new(1);

pub struct FlatMemory {
    data: Vec<u8>,
    mask: u64,
    epoch: u64,
}

impl FlatMemory {
    pub fn new(size_bytes: usize) -> Self {
        assert!(
            size_bytes.is_power_of_two(),
            "RAM size must be a power of two"
        );
        Self {
            data: vec![0u8; size_bytes],
            mask: (size_bytes - 1) as u64,
            epoch: FLAT_EPOCH_SOURCE.fetch_add(1, Ordering::Relaxed),
        }
    }

    pub fn load_at(&mut self, offset: usize, bytes: &[u8]) {
        self.data[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    #[inline(always)]
    fn idx(&self, address: u64) -> usize {
        (address & self.mask) as usize
    }
}

impl SystemBus for FlatMemory {
    #[inline(always)]
    fn read_byte(&mut self, address: u64) -> u8 {
        self.data[self.idx(address)]
    }

    #[inline(always)]
    fn read_halfword(&mut self, address: u64) -> u16 {
        let i = self.idx(address);
        u16::from_le_bytes(self.data[i..i + 2].try_into().unwrap())
    }

    #[inline(always)]
    fn read_word(&mut self, address: u64) -> u32 {
        let i = self.idx(address);
        u32::from_le_bytes(self.data[i..i + 4].try_into().unwrap())
    }

    #[inline(always)]
    fn read_doubleword(&mut self, address: u64) -> u64 {
        let i = self.idx(address);
        u64::from_le_bytes(self.data[i..i + 8].try_into().unwrap())
    }

    #[inline(always)]
    fn write_byte(&mut self, address: u64, val: u8) {
        let i = self.idx(address);
        self.data[i] = val;
    }

    #[inline(always)]
    fn write_halfword(&mut self, address: u64, val: u16) {
        let i = self.idx(address);
        self.data[i..i + 2].copy_from_slice(&val.to_le_bytes());
    }

    #[inline(always)]
    fn write_word(&mut self, address: u64, val: u32) {
        let i = self.idx(address);
        self.data[i..i + 4].copy_from_slice(&val.to_le_bytes());
    }

    #[inline(always)]
    fn write_doubleword(&mut self, address: u64, val: u64) {
        let i = self.idx(address);
        self.data[i..i + 8].copy_from_slice(&val.to_le_bytes());
    }

    fn ram_load_page(&mut self, address: u64) -> Option<*const u8> {
        if self.data.len() < 0x1000 {
            return None;
        }

        let page_index = self.idx(address) & !0xfff;
        Some(self.data[page_index..].as_ptr())
    }

    fn ram_store_page(&mut self, address: u64) -> Option<*mut u8> {
        if self.data.len() < 0x1000 {
            return None;
        }

        let page_index = self.idx(address) & !0xfff;
        Some(self.data[page_index..].as_mut_ptr())
    }

    fn ram_epoch(&self) -> u64 {
        self.epoch
    }
}
