// Manage and store the system state, the privilege level (Machine, User) and the MMU configuration.

// M-mode addresses
pub const MSTATUS: u32 = 0x300;
pub const MISA: u32 = 0x301;
pub const MEDELEG: u32 = 0x302;
pub const MIDELEG: u32 = 0x303;
pub const MIE: u32 = 0x304;
pub const MTVEC: u32 = 0x305;
pub const MCOUNTEREN: u32 = 0x306;
pub const MCOUNTINHIBIT: u32 = 0x320;
pub const MSCRATCH: u32 = 0x340;
pub const MEPC: u32 = 0x341;
pub const MCAUSE: u32 = 0x342;
pub const MTVAL: u32 = 0x343;
pub const MIP: u32 = 0x344;
pub const MENVCFG: u32 = 0x30A;
pub const MCONFIGPTR: u32 = 0xF15;

// S-mode addresses
pub const SSTATUS: u32 = 0x100;
pub const SIE: u32 = 0x104;
pub const STVEC: u32 = 0x105;
pub const SCOUNTEREN: u32 = 0x106;
pub const SENVCFG: u32 = 0x10A;
pub const SSCRATCH: u32 = 0x140;
pub const SEPC: u32 = 0x141;
pub const SCAUSE: u32 = 0x142;
pub const STVAL: u32 = 0x143;
pub const SIP: u32 = 0x144;
pub const SATP: u32 = 0x180;

// Counters
pub const CYCLE: u32 = 0xC00;
pub const TIME: u32 = 0xC01;
pub const INSTRET: u32 = 0xC02;
pub const MCYCLE: u32 = 0xB00;
pub const MINSTRET: u32 = 0xB02;

// Machine info
pub const MVENDORID: u32 = 0xF11;
pub const MARCHID: u32 = 0xF12;
pub const MIMPID: u32 = 0xF13;
pub const MHARTID: u32 = 0xF14;

// mstatus field masks for RV64
pub const MSTATUS_SIE: u64 = 1 << 1;
pub const MSTATUS_MIE: u64 = 1 << 3;
pub const MSTATUS_SPIE: u64 = 1 << 5;
pub const MSTATUS_UBE: u64 = 1 << 6;
pub const MSTATUS_MPIE: u64 = 1 << 7;
pub const MSTATUS_SPP: u64 = 1 << 8;
pub const MSTATUS_VS: u64 = 3 << 9;
pub const MSTATUS_MPP: u64 = 3 << 11;
pub const MSTATUS_FS: u64 = 3 << 13;
pub const MSTATUS_XS: u64 = 3 << 15;
pub const MSTATUS_MPRV: u64 = 1 << 17;
pub const MSTATUS_SUM: u64 = 1 << 18;
pub const MSTATUS_MXR: u64 = 1 << 19;
pub const MSTATUS_TVM: u64 = 1 << 20;
pub const MSTATUS_TW: u64 = 1 << 21;
pub const MSTATUS_TSR: u64 = 1 << 22;
pub const MSTATUS_UXL: u64 = 3 << 32;
pub const MSTATUS_SXL: u64 = 3 << 34;
pub const MSTATUS_SBE: u64 = 1 << 36;
pub const MSTATUS_MBE: u64 = 1 << 37;
pub const MSTATUS_SD: u64 = 1 << 63;

// mip / mie bits
pub const MIP_SSIP: u64 = 1 << 1;
pub const MIP_MSIP: u64 = 1 << 3;
pub const MIP_STIP: u64 = 1 << 5;
pub const MIP_MTIP: u64 = 1 << 7;
pub const MIP_SEIP: u64 = 1 << 9;
pub const MIP_MEIP: u64 = 1 << 11;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivMode {
    U = 0,
    S = 1,
    M = 3,
}

impl PrivMode {
    pub fn from_bits(bits: u64) -> Self {
        match bits & 3 {
            0 => PrivMode::U,
            1 => PrivMode::S,
            _ => PrivMode::M,
        }
    }
}

const MSTATUS_WRITE_MASK: u64 = MSTATUS_SIE
    | MSTATUS_MIE
    | MSTATUS_SPIE
    | MSTATUS_UBE
    | MSTATUS_MPIE
    | MSTATUS_SPP
    | MSTATUS_VS
    | MSTATUS_MPP
    | MSTATUS_FS
    | MSTATUS_XS
    | MSTATUS_MPRV
    | MSTATUS_SUM
    | MSTATUS_MXR
    | MSTATUS_TVM
    | MSTATUS_TW
    | MSTATUS_TSR;

const SSTATUS_MASK: u64 = MSTATUS_SIE
    | MSTATUS_SPIE
    | MSTATUS_UBE
    | MSTATUS_SPP
    | MSTATUS_VS
    | MSTATUS_FS
    | MSTATUS_XS
    | MSTATUS_SUM
    | MSTATUS_MXR;

// Bits writable via SIP (S-mode software interrupt)
const SIP_WRITABLE: u64 = MIP_SSIP;

pub struct Csr {
    pub mstatus: u64,
    pub misa: u64,
    pub medeleg: u64,
    pub mideleg: u64,
    pub mie: u64,
    pub mtvec: u64,
    pub mcounteren: u64,
    pub mcountinhibit: u64,
    pub mscratch: u64,
    pub mepc: u64,
    pub mcause: u64,
    pub mtval: u64,
    pub mip: u64,
    pub menvcfg: u64,

    pub stvec: u64,
    pub scounteren: u64,
    pub senvcfg: u64,
    pub sscratch: u64,
    pub sepc: u64,
    pub scause: u64,
    pub stval: u64,
    pub satp: u64,

    pub hart_id: u64,

    pub pmpcfg: [u64; 16],
    pub pmpaddr: [u64; 64],

    pub cycle: u64,
    pub instret: u64,
    pub time: u64,

    // FP CSRs
    pub fcsr: u64,

    // Vector CSRs
    // HPM event selectors (3..31)
    pub mhpmevent: [u64; 29],
}

impl Default for Csr {
    fn default() -> Self {
        Self::new()
    }
}

impl Csr {
    pub fn new() -> Self {
        let misa = (2u64 << 62)
            | (1 << 0)   // A extension
            | (1 << 2)   // C extension
            | (1 << 3)   // D extension
            | (1 << 5)   // F extension
            | (1 << 8)   // I extension
            | (1 << 12)  // M extension
            | (1 << 18)  // S extension
            | (1 << 20); // U extension

        Self {
            misa,
            mstatus: (2u64 << 32) | (2u64 << 34),
            medeleg: 0,
            mideleg: 0,
            mie: 0,
            mtvec: 0,
            mcounteren: 0,
            mcountinhibit: 0,
            mscratch: 0,
            mepc: 0,
            mcause: 0,
            mtval: 0,
            mip: 0,
            menvcfg: 0,
            stvec: 0,
            scounteren: 0,
            senvcfg: 0,
            sscratch: 0,
            sepc: 0,
            scause: 0,
            stval: 0,
            satp: 0,
            hart_id: 0,
            pmpcfg: [0; 16],
            pmpaddr: [0; 64],
            cycle: 0,
            instret: 0,
            time: 0,
            fcsr: 0,
            mhpmevent: [0; 29],
        }
    }

    pub fn read_register(&self, address: u32, priv_mode: PrivMode) -> Option<u64> {
        let value = match address {
            // M-mode
            MSTATUS => self.read_mstatus(),
            MISA => self.misa,
            MEDELEG => self.medeleg,
            MIDELEG => self.mideleg,
            MIE => self.mie,
            MTVEC => self.mtvec,
            MCOUNTEREN => self.mcounteren,
            MCOUNTINHIBIT => self.mcountinhibit,
            MENVCFG => self.menvcfg,
            MSCRATCH => self.mscratch,
            MEPC => self.mepc,
            MCAUSE => self.mcause,
            MTVAL => self.mtval,
            MIP => self.mip,

            // S-mode
            SSTATUS => self.read_mstatus() & SSTATUS_MASK,
            SIE => self.mie & self.mideleg,
            STVEC => self.stvec,
            SCOUNTEREN => self.scounteren,
            SENVCFG => self.senvcfg,
            SSCRATCH => self.sscratch,
            SEPC => self.sepc,
            SCAUSE => self.scause,
            STVAL => self.stval,
            SIP => self.mip & self.mideleg,
            SATP => {
                if priv_mode == PrivMode::S && (self.mstatus & MSTATUS_TVM) != 0 {
                    return None;
                }
                self.satp
            }

            // Machine info
            MVENDORID | MARCHID | MIMPID => 0,
            MHARTID => self.hart_id,
            MCONFIGPTR => 0,

            // PMP
            0x3A0..=0x3AF => self.pmpcfg[(address - 0x3A0) as usize],
            0x3B0..=0x3EF => self.pmpaddr[(address - 0x3B0) as usize],

            // Counters (U/S readable)
            CYCLE | MCYCLE => self.cycle,
            TIME => self.time,
            INSTRET | MINSTRET => self.instret,

            // HPM counters 3-31
            0xB03..=0xB1F => 0,
            // HPM counters upper 32
            0xB83..=0xB9F => 0,
            // User HPM counters
            0xC03..=0xC1F => 0,
            0xC83..=0xC9F => 0,
            // cycleh, timeh, instreth
            0xC80..=0xC82 => 0,

            // FP CSRs
            0x001 => self.fcsr & 0x1F,       // fflags
            0x002 => (self.fcsr >> 5) & 0x7, // frm
            0x003 => self.fcsr & 0xFF,       // fcsr

            // mhpmevent3-31
            0x323..=0x33F => self.mhpmevent[(address - 0x323) as usize],

            // scountovf
            0xDA0 => 0,

            // mseccfg
            0x747 => 0,

            // tselect, tdata1-3
            0x7A0 => 0,
            0x7A1 => 0,
            0x7A2 => 0,
            0x7A3 => 0,

            _ => return None,
        };
        Some(value)
    }

    pub fn write_register(&mut self, address: u32, value: u64, priv_mode: PrivMode) -> bool {
        match address {
            // M-mode
            MSTATUS => self.write_mstatus(value),
            MISA => {} // read-only
            MEDELEG => self.medeleg = value,
            MIDELEG => self.mideleg = value,
            MIE => self.mie = value,
            MTVEC => self.mtvec = value,
            MCOUNTEREN => self.mcounteren = value & 0xFFFF_FFFF,
            MCOUNTINHIBIT => self.mcountinhibit = value & 0xFFFF_FFFF,
            MENVCFG => self.menvcfg = value,
            MSCRATCH => self.mscratch = value,
            MEPC => self.mepc = value & !1,
            MCAUSE => self.mcause = value,
            MTVAL => self.mtval = value,
            MIP => {
                // Only MSIP, STIP, SSIP are writable from M-mode software
                let writable = MIP_MSIP | MIP_SSIP | MIP_STIP;
                self.mip = (self.mip & !writable) | (value & writable);
            }

            // S-mode
            SSTATUS => {
                let mask = SSTATUS_MASK & MSTATUS_WRITE_MASK;
                self.mstatus = (self.mstatus & !mask) | (value & mask);
            }
            SIE => {
                let mask = self.mideleg;
                self.mie = (self.mie & !mask) | (value & mask);
            }
            STVEC => self.stvec = value,
            SCOUNTEREN => self.scounteren = value & 0xFFFF_FFFF,
            SENVCFG => self.senvcfg = value,
            SSCRATCH => self.sscratch = value,
            SEPC => self.sepc = value & !1,
            SCAUSE => self.scause = value,
            STVAL => self.stval = value,
            SIP => {
                let mask = self.mideleg & SIP_WRITABLE;
                self.mip = (self.mip & !mask) | (value & mask);
            }
            SATP => {
                if priv_mode == PrivMode::S && (self.mstatus & MSTATUS_TVM) != 0 {
                    return false;
                }
                self.satp = value;
            }

            // PMP
            0x3A0..=0x3AF => self.pmpcfg[(address - 0x3A0) as usize] = value,
            0x3B0..=0x3EF => self.pmpaddr[(address - 0x3B0) as usize] = value,

            // Counters
            MCYCLE => self.cycle = value,
            MINSTRET => self.instret = value,

            // FP CSRs
            0x001 => self.fcsr = (self.fcsr & !0x1F) | (value & 0x1F),
            0x002 => self.fcsr = (self.fcsr & !0xE0) | ((value & 0x7) << 5),
            0x003 => self.fcsr = value & 0xFF,

            // HPM events
            0x323..=0x33F => self.mhpmevent[(address - 0x323) as usize] = value,

            // HPM counters (ignore writes)
            0xB03..=0xB1F | 0xB83..=0xB9F => {}

            // Read-only from U/S
            CYCLE | INSTRET | TIME => {}
            0xC03..=0xC1F | 0xC83..=0xC9F | 0xC80..=0xC82 => {}

            // Read-only machine info
            MVENDORID | MARCHID | MIMPID | MHARTID | MCONFIGPTR => {}

            // scountovf, mseccfg, tselect, tdata1-3
            0xDA0 | 0x747 | 0x7A0..=0x7A3 => {}

            _ => return false,
        }
        true
    }

    fn read_mstatus(&self) -> u64 {
        let mut value = self.mstatus;

        let fs = (value >> 13) & 3;
        let xs = (value >> 15) & 3;
        let vs = (value >> 9) & 3;

        if fs == 3 || xs == 3 || vs == 3 {
            value |= MSTATUS_SD;
        } else {
            value &= !MSTATUS_SD;
        }

        value
    }

    fn write_mstatus(&mut self, value: u64) {
        let preserved = MSTATUS_UXL | MSTATUS_SXL;
        self.mstatus = (self.mstatus & preserved) | (value & MSTATUS_WRITE_MASK);
    }

    #[inline(always)]
    pub fn pending_interrupt(&self, priv_mode: PrivMode) -> Option<u64> {
        let pending = self.mip & self.mie;
        if pending == 0 {
            return None;
        }
        self.pending_interrupt_slow(priv_mode, pending)
    }

    #[cold]
    #[inline(never)]
    fn pending_interrupt_slow(&self, priv_mode: PrivMode, pending: u64) -> Option<u64> {
        let mie_bit = (self.mstatus & MSTATUS_MIE) != 0;
        let sie_bit = (self.mstatus & MSTATUS_SIE) != 0;

        // M-mode interrupts: not delegated
        let m_pending = pending & !self.mideleg;
        if m_pending != 0 && (priv_mode != PrivMode::M || mie_bit) {
            return Some(highest_priority_interrupt_bit(m_pending));
        }

        // S-mode interrupts: delegated
        let s_pending = pending & self.mideleg;
        if s_pending != 0 && (priv_mode == PrivMode::U || (priv_mode == PrivMode::S && sie_bit)) {
            return Some(highest_priority_interrupt_bit(s_pending));
        }

        None
    }
}

fn highest_priority_interrupt_bit(value: u64) -> u64 {
    // Priority order: MEI, MSI, MTI, SEI, SSI, STI (11, 3, 7, 9, 1, 5)
    for bit in [11u64, 3, 7, 9, 1, 5] {
        if value & (1 << bit) != 0 {
            return bit;
        }
    }
    63 - value.leading_zeros() as u64
}
