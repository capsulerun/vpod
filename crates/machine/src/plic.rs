pub const PLIC_BASE: u64 = 0x4010_0000;
pub const PLIC_SIZE: u64 = 0x0040_0000;

const PLIC_HART_BASE: u64 = 0x0020_0000;

pub struct Plic {
    pending: u32,
    served: u32,
}

impl Default for Plic {
    fn default() -> Self {
        Self::new()
    }
}

impl Plic {
    pub fn new() -> Self {
        Self {
            pending: 0,
            served: 0,
        }
    }

    pub fn save(&self, w: &mut impl std::io::Write) -> std::io::Result<()> {
        w.write_all(&(self.pending as u64).to_le_bytes())?;
        w.write_all(&(self.served as u64).to_le_bytes())?;

        Ok(())
    }

    pub fn restore(&mut self, r: &mut impl std::io::Read) -> std::io::Result<()> {
        let mut b = [0u8; 8];
        r.read_exact(&mut b)?; self.pending = u64::from_le_bytes(b) as u32;
        r.read_exact(&mut b)?; self.served = u64::from_le_bytes(b) as u32;

        Ok(())
    }

    pub fn set_irq(&mut self, irq: u32, level: bool) {
        assert!((1..=31).contains(&irq));
        if level {
            self.pending |= 1 << irq;
        } else {
            self.pending &= !(1 << irq);
        }
    }

    pub fn irq_pending(&self) -> bool {
        self.pending & !self.served != 0
    }

    pub fn read(&mut self, offset: u64) -> u32 {
        match offset {
            o if o == PLIC_HART_BASE => 0,
            o if o == PLIC_HART_BASE + 4 => {
                let mask = self.pending & !self.served;
                if mask != 0 {
                    let irq = mask.trailing_zeros();
                    self.served |= 1 << irq;
                    irq
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u64, val: u32) {
        match offset {
            o if o == PLIC_HART_BASE => {}
            o if o == PLIC_HART_BASE + 4 => {
                if (1..=31).contains(&val) {
                    self.served &= !(1 << val);
                }
            }
            _ => {}
        }
    }
}
