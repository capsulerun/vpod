pub const CLINT_BASE: u64 = 0x0200_0000;
pub const CLINT_SIZE: u64 = 0x000c_0000;

pub const RTC_FREQ: u64 = 10_000_000;

const INSNS_PER_TICK: u64 = 10;

pub struct Clint {
    pub mtimecmp: u64,
    pub msip: u32,
    mtime: u64,
    insn_counter: u64,
}

impl Default for Clint {
    fn default() -> Self {
        Self::new()
    }
}

impl Clint {
    pub fn new() -> Self {
        Self {
            mtimecmp: u64::MAX,
            msip: 0,
            mtime: 0,
            insn_counter: 0,
        }
    }

    pub fn advance(&mut self, insns: u64) {
        self.insn_counter += insns;
        self.mtime += self.insn_counter / INSNS_PER_TICK;
        self.insn_counter %= INSNS_PER_TICK;
    }

    pub fn mtime(&self) -> u64 {
        self.mtime
    }

    pub fn tick(&self) -> (bool, bool) {
        (self.mtime >= self.mtimecmp, self.msip & 1 != 0)
    }

    pub fn read(&self, offset: u64) -> u32 {
        match offset {
            0x0000 => self.msip,
            0xbff8 => self.mtime as u32,
            0xbffc => (self.mtime >> 32) as u32,
            0x4000 => self.mtimecmp as u32,
            0x4004 => (self.mtimecmp >> 32) as u32,
            _ => 0,
        }
    }

    pub fn save(&self, w: &mut impl std::io::Write) -> std::io::Result<()> {
        w.write_all(&self.mtimecmp.to_le_bytes())?;
        w.write_all(&(self.msip as u64).to_le_bytes())?;
        w.write_all(&self.mtime.to_le_bytes())?;
        w.write_all(&self.insn_counter.to_le_bytes())?;

        Ok(())
    }

    pub fn restore(&mut self, r: &mut impl std::io::Read) -> std::io::Result<()> {
        let mut b = [0u8; 8];
        r.read_exact(&mut b)?; self.mtimecmp = u64::from_le_bytes(b);
        r.read_exact(&mut b)?; self.msip = u64::from_le_bytes(b) as u32;
        r.read_exact(&mut b)?; self.mtime = u64::from_le_bytes(b);
        r.read_exact(&mut b)?; self.insn_counter = u64::from_le_bytes(b);

        Ok(())
    }

    pub fn write(&mut self, offset: u64, val: u32) {
        match offset {
            0x0000 => self.msip = val & 1,
            0x4000 => {
                self.mtimecmp = (self.mtimecmp & 0xffff_ffff_0000_0000) | val as u64;
            }
            0x4004 => {
                self.mtimecmp = (self.mtimecmp & 0x0000_0000_ffff_ffff) | ((val as u64) << 32);
            }
            _ => {}
        }
    }
}
