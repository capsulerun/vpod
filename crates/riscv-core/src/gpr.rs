// Fast working memory for the execution unit.

pub struct Gpr {
    x: [u64; 32],
    pub pc: u64,
    pub f: [u64; 32],
}

impl Gpr {
    pub fn new(pc: u64) -> Self {
        Self {
            x: [0u64; 32],
            pc,
            f: [0xFFFF_FFFF_FFFF_FFFFu64; 32],
        }
    }

    #[inline(always)]
    pub fn read(&self, reg: usize) -> u64 {
        if reg == 0 {
            0
        } else {
            self.x[reg]
        }
    }

    #[inline(always)]
    pub fn read_f(&self, reg: usize) -> u64 {
        self.f[reg]
    }

    #[inline(always)]
    pub fn write(&mut self, reg: usize, val: u64) {
        if reg != 0 {
            self.x[reg] = val;
        }
    }


    #[inline(always)]
    pub fn write_f(&mut self, reg: usize, val: u64) {
        self.f[reg] = val;
    }

    #[inline(always)]
    pub fn write_f32(&mut self, reg: usize, val: u32) {
        self.f[reg] = 0xFFFF_FFFF_0000_0000 | val as u64;
    }

    #[inline(always)]
    pub fn read_f32(&self, reg: usize) -> u32 {
        let bits = self.f[reg];

        if bits & 0xFFFF_FFFF_0000_0000 == 0xFFFF_FFFF_0000_0000 {
            bits as u32
        } else {
            0x7FC0_0000 // equal to NaN
        }
    }
}
