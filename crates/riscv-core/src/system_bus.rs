// External communication linking the hart to RAM and peripherals (disk, network).

pub trait SystemBus {
    fn read_byte(&mut self, addr: u64) -> u8;
    fn read_halfword(&mut self, addr: u64) -> u16;
    fn read_word(&mut self, addr: u64) -> u32;
    fn read_doubleword(&mut self, addr: u64) -> u64;

    fn write_byte(&mut self, addr: u64, val: u8);
    fn write_halfword(&mut self, addr: u64, val: u16);
    fn write_word(&mut self, addr: u64, val: u32);
    fn write_doubleword(&mut self, addr: u64, val: u64);
}

pub struct FlatMemory {
    data: Vec<u8>,
    mask: u64,
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
        }
    }

    pub fn load_at(&mut self, offset: usize, bytes: &[u8]) {
        self.data[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    #[inline(always)]
    fn idx(&self, addr: u64) -> usize {
        (addr & self.mask) as usize
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
}
