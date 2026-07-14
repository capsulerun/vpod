pub const CLINT_BASE: u64 = 0x0200_0000;
pub const CLINT_SIZE: u64 = 0x000c_0000;

pub const TIMER_FREQUENCY: u64 = 10_000_000;

const INSTRUCTIONS_PER_TICK: u64 = 10;

pub struct Clint {
    pub mtimecmp: u64,
    pub msip: u32,
    mtime: u64,
    instruction_counter: u64,
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
            instruction_counter: 0,
        }
    }

    pub fn advance_by_instructions(&mut self, instructions: u64) {
        self.instruction_counter += instructions;
        self.mtime += self.instruction_counter / INSTRUCTIONS_PER_TICK;
        self.instruction_counter %= INSTRUCTIONS_PER_TICK;
    }

    pub fn advance_by_nanos(&mut self, nanos: u64) {
        const NANOS_PER_TICK: u64 = 1_000_000_000 / TIMER_FREQUENCY;
        self.mtime += nanos / NANOS_PER_TICK;
    }

    pub fn mtime(&self) -> u64 {
        self.mtime
    }

    pub fn get_interrupt_status(&self) -> (bool, bool) {
        (self.mtime >= self.mtimecmp, self.msip & 1 != 0)
    }

    pub fn read_register(&self, offset: u64) -> u32 {
        match offset {
            0x0000 => self.msip,
            0xbff8 => self.mtime as u32,
            0xbffc => (self.mtime >> 32) as u32,
            0x4000 => self.mtimecmp as u32,
            0x4004 => (self.mtimecmp >> 32) as u32,
            _ => 0,
        }
    }

    pub fn serialize(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        writer.write_all(&self.mtimecmp.to_le_bytes())?;
        writer.write_all(&(self.msip as u64).to_le_bytes())?;
        writer.write_all(&self.mtime.to_le_bytes())?;
        writer.write_all(&self.instruction_counter.to_le_bytes())?;

        Ok(())
    }

    pub fn deserialize(&mut self, reader: &mut impl std::io::Read) -> std::io::Result<()> {
        let mut bytes = [0u8; 8];

        reader.read_exact(&mut bytes)?;
        self.mtimecmp = u64::from_le_bytes(bytes);

        reader.read_exact(&mut bytes)?;
        self.msip = u64::from_le_bytes(bytes) as u32;

        reader.read_exact(&mut bytes)?;
        self.mtime = u64::from_le_bytes(bytes);

        reader.read_exact(&mut bytes)?;
        self.instruction_counter = u64::from_le_bytes(bytes);

        Ok(())
    }

    pub fn write_register(&mut self, offset: u64, val: u32) {
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
