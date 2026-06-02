pub mod blk;
pub mod console;
pub mod net;
pub mod slirp;

use crate::RAM_BASE;

// MMIO v2 register offsets
pub const MMIO_MAGIC: u64 = 0x000;
pub const MMIO_VERSION: u64 = 0x004;
pub const MMIO_DEVICE_ID: u64 = 0x008;
pub const MMIO_VENDOR_ID: u64 = 0x00c;
pub const MMIO_DEVICE_FEATURES: u64 = 0x010;
pub const MMIO_DEVICE_FEATURES_SEL: u64 = 0x014;
pub const MMIO_DRIVER_FEATURES: u64 = 0x020;
pub const MMIO_DRIVER_FEATURES_SEL: u64 = 0x024;
pub const MMIO_QUEUE_SEL: u64 = 0x030;
pub const MMIO_QUEUE_NUM_MAX: u64 = 0x034;
pub const MMIO_QUEUE_NUM: u64 = 0x038;
pub const MMIO_QUEUE_READY: u64 = 0x044;
pub const MMIO_QUEUE_NOTIFY: u64 = 0x050;
pub const MMIO_INTERRUPT_STATUS: u64 = 0x060;
pub const MMIO_INTERRUPT_ACK: u64 = 0x064;
pub const MMIO_STATUS: u64 = 0x070;
pub const MMIO_QUEUE_DESC_LOW: u64 = 0x080;
pub const MMIO_QUEUE_DESC_HIGH: u64 = 0x084;
pub const MMIO_QUEUE_AVAIL_LOW: u64 = 0x090;
pub const MMIO_QUEUE_AVAIL_HIGH: u64 = 0x094;
pub const MMIO_QUEUE_USED_LOW: u64 = 0x0a0;
pub const MMIO_QUEUE_USED_HIGH: u64 = 0x0a4;
pub const MMIO_CONFIG_GENERATION: u64 = 0x0fc;
pub const MMIO_CONFIG: u64 = 0x100;

const MMIO_MAGIC_VALUE: u32 = 0x7472_6976;
const MMIO_VERSION_VALUE: u32 = 2;
const QUEUE_NUM_MAX: u32 = 256;
const MAX_QUEUES: usize = 4;

pub const VRING_DESC_F_NEXT: u16 = 1;
pub const VRING_DESC_F_WRITE: u16 = 2;

pub struct RamView<'a> {
    ram: &'a mut Vec<u8>,
    mask: u64,
}

impl<'a> RamView<'a> {
    pub fn new(ram: &'a mut Vec<u8>, mask: u64) -> Self {
        Self { ram, mask }
    }

    fn idx(&self, pa: u64) -> usize {
        ((pa - RAM_BASE) & self.mask) as usize
    }

    pub fn read_u8(&self, pa: u64) -> u8 {
        self.ram[self.idx(pa)]
    }

    pub fn read_u16(&self, pa: u64) -> u16 {
        let i = self.idx(pa);
        u16::from_le_bytes(self.ram[i..i + 2].try_into().unwrap())
    }

    pub fn read_u32(&self, pa: u64) -> u32 {
        let i = self.idx(pa);
        u32::from_le_bytes(self.ram[i..i + 4].try_into().unwrap())
    }

    pub fn read_u64(&self, pa: u64) -> u64 {
        let i = self.idx(pa);
        u64::from_le_bytes(self.ram[i..i + 8].try_into().unwrap())
    }

    pub fn write_u8(&mut self, pa: u64, val: u8) {
        let i = self.idx(pa);
        self.ram[i] = val;
    }

    pub fn write_u16(&mut self, pa: u64, val: u16) {
        let i = self.idx(pa);
        self.ram[i..i + 2].copy_from_slice(&val.to_le_bytes());
    }

    pub fn write_u32(&mut self, pa: u64, val: u32) {
        let i = self.idx(pa);
        self.ram[i..i + 4].copy_from_slice(&val.to_le_bytes());
    }

    pub fn read_bytes(&self, pa: u64, buf: &mut [u8]) {
        let i = self.idx(pa);
        buf.copy_from_slice(&self.ram[i..i + buf.len()]);
    }

    pub fn write_bytes(&mut self, pa: u64, buf: &[u8]) {
        let i = self.idx(pa);
        self.ram[i..i + buf.len()].copy_from_slice(buf);
    }
}

#[derive(Default)]
pub struct VirtQueue {
    pub ready: bool,
    pub num: u32,
    pub last_avail_idx: u16,
    pub desc_addr: u64,
    pub avail_addr: u64,
    pub used_addr: u64,
}

pub struct VirtDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

impl VirtQueue {
    pub fn read_desc(&self, ram: &RamView, idx: u16) -> VirtDesc {
        let base = self.desc_addr + idx as u64 * 16;
        VirtDesc {
            addr: ram.read_u64(base),
            len: ram.read_u32(base + 8),
            flags: ram.read_u16(base + 12),
            next: ram.read_u16(base + 14),
        }
    }

    pub fn pop_avail(&mut self, ram: &RamView) -> Option<u16> {
        if !self.ready {
            return None;
        }

        let avail_idx = ram.read_u16(self.avail_addr + 2);
        if self.last_avail_idx == avail_idx {
            return None;
        }

        let ring_slot = self.last_avail_idx & (self.num as u16 - 1);
        let desc_idx = ram.read_u16(self.avail_addr + 4 + ring_slot as u64 * 2);

        self.last_avail_idx = self.last_avail_idx.wrapping_add(1);
        Some(desc_idx)
    }

    pub fn push_used(&self, ram: &mut RamView, desc_idx: u16, len: u32) {
        let used_idx = ram.read_u16(self.used_addr + 2);
        let slot = used_idx & (self.num as u16 - 1);
        let entry_addr = self.used_addr + 4 + slot as u64 * 8;

        ram.write_u32(entry_addr, desc_idx as u32);
        ram.write_u32(entry_addr + 4, len);
        ram.write_u16(self.used_addr + 2, used_idx.wrapping_add(1));
    }
}

macro_rules! lo {
    ($field:expr) => {
        $field as u32
    };
}

macro_rules! hi {
    ($field:expr) => {
        ($field >> 32) as u32
    };
}

macro_rules! set_lo {
    ($field:expr, $val:expr) => {
        $field = ($field & !0xffff_ffff) | $val as u64
    };
}

macro_rules! set_hi {
    ($field:expr, $val:expr) => {
        $field = ($field & 0xffff_ffff) | ($val as u64) << 32
    };
}

pub struct VirtioMmio {
    pub device_id: u32,
    device_features: u64,
    driver_features: u64,
    device_features_sel: u32,
    driver_features_sel: u32,

    pub status: u32,
    pub int_status: u32,
    pub queue_sel: usize,
    pub queues: [VirtQueue; MAX_QUEUES],
    pub config: [u8; 64],
    pub config_gen: u32,
    num_queues: usize,
}

impl VirtioMmio {
    pub fn new(device_id: u32, device_features: u64, num_queues: usize) -> Self {
        Self {
            device_id,
            device_features,
            driver_features: 0,
            device_features_sel: 0,
            driver_features_sel: 0,
            status: 0,
            int_status: 0,
            queue_sel: 0,
            queues: std::array::from_fn(|_| VirtQueue::default()),
            config: [0; 64],
            config_gen: 0,
            num_queues,
        }
    }

    pub fn read(&self, offset: u64) -> u32 {
        if offset >= MMIO_CONFIG {
            let i = (offset - MMIO_CONFIG) as usize;

            if i + 3 < self.config.len() {
                return u32::from_le_bytes(self.config[i..i + 4].try_into().unwrap());
            }

            return 0;
        }
        let q = &self.queues[self.queue_sel.min(self.num_queues - 1)];
        match offset {
            MMIO_MAGIC => MMIO_MAGIC_VALUE,
            MMIO_VERSION => MMIO_VERSION_VALUE,
            MMIO_DEVICE_ID => self.device_id,
            MMIO_VENDOR_ID => 0xffff_ffff,
            MMIO_DEVICE_FEATURES => {
                if self.device_features_sel == 0 {
                    self.device_features as u32
                } else {
                    (self.device_features >> 32) as u32
                }
            }
            MMIO_QUEUE_NUM_MAX => QUEUE_NUM_MAX,
            MMIO_QUEUE_READY => q.ready as u32,
            MMIO_INTERRUPT_STATUS => self.int_status,
            MMIO_STATUS => self.status,
            MMIO_QUEUE_DESC_LOW => lo!(q.desc_addr),
            MMIO_QUEUE_DESC_HIGH => hi!(q.desc_addr),
            MMIO_QUEUE_AVAIL_LOW => lo!(q.avail_addr),
            MMIO_QUEUE_AVAIL_HIGH => hi!(q.avail_addr),
            MMIO_QUEUE_USED_LOW => lo!(q.used_addr),
            MMIO_QUEUE_USED_HIGH => hi!(q.used_addr),
            MMIO_CONFIG_GENERATION => self.config_gen,
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u64, val: u32) -> Option<usize> {
        if offset >= MMIO_CONFIG {
            let i = (offset - MMIO_CONFIG) as usize;
            if i + 3 < self.config.len() {
                self.config[i..i + 4].copy_from_slice(&val.to_le_bytes());
            }
            return None;
        }

        let sel = self.queue_sel.min(self.num_queues - 1);
        match offset {
            MMIO_DEVICE_FEATURES_SEL => self.device_features_sel = val,
            MMIO_DRIVER_FEATURES => {
                if self.driver_features_sel == 0 {
                    self.driver_features = (self.driver_features & !0xffff_ffff) | val as u64;
                } else {
                    self.driver_features =
                        (self.driver_features & 0xffff_ffff) | (val as u64) << 32;
                }
            }
            MMIO_DRIVER_FEATURES_SEL => self.driver_features_sel = val,
            MMIO_QUEUE_SEL => self.queue_sel = val as usize % self.num_queues,
            MMIO_QUEUE_NUM => self.queues[sel].num = val,
            MMIO_QUEUE_READY => self.queues[sel].ready = val != 0,
            MMIO_QUEUE_NOTIFY => return Some(val as usize % self.num_queues),
            MMIO_INTERRUPT_ACK => self.int_status &= !val,
            MMIO_STATUS => {
                self.status = val;
                if val == 0 {
                    self.reset();
                }
            }
            MMIO_QUEUE_DESC_LOW => set_lo!(self.queues[sel].desc_addr, val),
            MMIO_QUEUE_DESC_HIGH => set_hi!(self.queues[sel].desc_addr, val),
            MMIO_QUEUE_AVAIL_LOW => set_lo!(self.queues[sel].avail_addr, val),
            MMIO_QUEUE_AVAIL_HIGH => set_hi!(self.queues[sel].avail_addr, val),
            MMIO_QUEUE_USED_LOW => set_lo!(self.queues[sel].used_addr, val),
            MMIO_QUEUE_USED_HIGH => set_hi!(self.queues[sel].used_addr, val),
            _ => {}
        }
        None
    }

    fn reset(&mut self) {
        self.driver_features = 0;
        self.int_status = 0;
        self.queue_sel = 0;

        for q in &mut self.queues {
            *q = VirtQueue::default();
        }
    }

    pub fn serialize(&self, w: &mut impl std::io::Write) -> std::io::Result<()> {
        w.write_all(&self.driver_features.to_le_bytes())?;
        w.write_all(&self.status.to_le_bytes())?;
        w.write_all(&self.int_status.to_le_bytes())?;
        w.write_all(&(self.queue_sel as u32).to_le_bytes())?;
        w.write_all(&(self.num_queues as u32).to_le_bytes())?;

        for q in &self.queues[..self.num_queues] {
            w.write_all(&[q.ready as u8])?;
            w.write_all(&q.num.to_le_bytes())?;
            w.write_all(&q.last_avail_idx.to_le_bytes())?;
            w.write_all(&q.desc_addr.to_le_bytes())?;
            w.write_all(&q.avail_addr.to_le_bytes())?;
            w.write_all(&q.used_addr.to_le_bytes())?;
        }

        w.write_all(&self.config)?;
        Ok(())
    }

    pub fn deserialize(&mut self, r: &mut impl std::io::Read) -> std::io::Result<()> {
        let mut buf8 = [0u8; 8];
        let mut buf4 = [0u8; 4];
        let mut buf2 = [0u8; 2];
        let mut buf1 = [0u8; 1];

        r.read_exact(&mut buf8)?;
        self.driver_features = u64::from_le_bytes(buf8);

        r.read_exact(&mut buf4)?;
        self.status = u32::from_le_bytes(buf4);

        r.read_exact(&mut buf4)?;
        self.int_status = u32::from_le_bytes(buf4);

        r.read_exact(&mut buf4)?;
        self.queue_sel = u32::from_le_bytes(buf4) as usize;

        r.read_exact(&mut buf4)?;
        let num_queues = u32::from_le_bytes(buf4) as usize;

        for q in &mut self.queues[..num_queues] {
            r.read_exact(&mut buf1)?;
            q.ready = buf1[0] != 0;

            r.read_exact(&mut buf4)?;
            q.num = u32::from_le_bytes(buf4);

            r.read_exact(&mut buf2)?;
            q.last_avail_idx = u16::from_le_bytes(buf2);

            r.read_exact(&mut buf8)?;
            q.desc_addr = u64::from_le_bytes(buf8);

            r.read_exact(&mut buf8)?;
            q.avail_addr = u64::from_le_bytes(buf8);

            r.read_exact(&mut buf8)?;
            q.used_addr = u64::from_le_bytes(buf8);
        }

        r.read_exact(&mut self.config)?;
        Ok(())
    }
}
