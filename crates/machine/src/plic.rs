pub const PLIC_BASE: u64 = 0x4010_0000;
pub const PLIC_SIZE: u64 = 0x0040_0000;

const PLIC_HART_BASE: u64 = 0x0020_0000;
const PLIC_PRIORITY_THRESHOLD_OFFSET: u64 = PLIC_HART_BASE;
const PLIC_CLAIM_COMPLETE_OFFSET: u64 = PLIC_HART_BASE + 4;

const MIN_IRQ: u32 = 1;
const MAX_IRQ: u32 = 31;

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

    pub fn save_state(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        writer.write_all(&(self.pending as u64).to_le_bytes())?;
        writer.write_all(&(self.served as u64).to_le_bytes())?;

        Ok(())
    }

    pub fn restore_state(&mut self, reader: &mut impl std::io::Read) -> std::io::Result<()> {
        let mut buffer = [0u8; 8];

        reader.read_exact(&mut buffer)?;
        self.pending = u64::from_le_bytes(buffer) as u32;

        reader.read_exact(&mut buffer)?;
        self.served = u64::from_le_bytes(buffer) as u32;

        Ok(())
    }

    pub fn set_irq(&mut self, irq_number: u32, level: bool) {
        assert!(
            (MIN_IRQ..=MAX_IRQ).contains(&irq_number),
            "IRQ number must be between {} and {}",
            MIN_IRQ,
            MAX_IRQ
        );

        if level {
            self.pending |= 1 << irq_number;
        } else {
            self.pending &= !(1 << irq_number);
        }
    }

    pub fn irq_pending(&self) -> bool {
        self.pending & !self.served != 0
    }

    pub fn read_register(&mut self, offset: u64) -> u32 {
        match offset {
            PLIC_PRIORITY_THRESHOLD_OFFSET => 0,

            PLIC_CLAIM_COMPLETE_OFFSET => {
                let unserviced_interrupts = self.pending & !self.served;

                if unserviced_interrupts != 0 {
                    let irq_number = unserviced_interrupts.trailing_zeros();
                    self.served |= 1 << irq_number;
                    irq_number
                } else {
                    0
                }
            }

            _ => 0,
        }
    }

    pub fn write_register(&mut self, offset: u64, value: u32) {
        match offset {
            PLIC_PRIORITY_THRESHOLD_OFFSET => {}

            PLIC_CLAIM_COMPLETE_OFFSET if (MIN_IRQ..=MAX_IRQ).contains(&value) => {
                self.served &= !(1 << value);
            }

            _ => {}
        }
    }
}
