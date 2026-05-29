use crate::csr::{Csr, PrivMode};
use crate::execute::{self, ExecContext};

#[cfg(not(target_arch = "wasm32"))]
use crate::execute::ICACHE_SIZE;

use crate::gpr::Gpr;
use crate::mmu::Mmu;
use crate::system_bus::SystemBus;
use crate::trap::StepResult;

pub const VLEN_BYTES: usize = 16;
pub const VREG_COUNT: usize = 32;

pub struct Hart {
    pub regs: Gpr,
    pub csr: Csr,
    pub mmu: Mmu,
    pub priv_mode: PrivMode,
    pub lr_addr: Option<u64>,
    pub vregs: Box<[[u8; VLEN_BYTES]; VREG_COUNT]>,
    pub fetch_vpage: u64,
    pub fetch_ppage: u64,
    pub fetch_satp: u64,

    #[cfg(not(target_arch = "wasm32"))]
    pub icache_tags: Box<[u64; ICACHE_SIZE]>,

    #[cfg(not(target_arch = "wasm32"))]
    pub icache_data: Box<[u32; ICACHE_SIZE]>,
}

impl Hart {
    pub fn new(entry: u64) -> Self {
        Self {
            regs: Gpr::new(entry),
            csr: Csr::new(),
            mmu: Mmu::new(),
            priv_mode: PrivMode::M,
            lr_addr: None,
            vregs: Box::new([[0u8; VLEN_BYTES]; VREG_COUNT]),
            fetch_vpage: u64::MAX,
            fetch_ppage: 0,
            fetch_satp: u64::MAX,

            #[cfg(not(target_arch = "wasm32"))]
            icache_tags: Box::new([u64::MAX; ICACHE_SIZE]),

            #[cfg(not(target_arch = "wasm32"))]
            icache_data: Box::new([0u32; ICACHE_SIZE]),
        }
    }

    pub fn invalidate_icache(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.icache_tags.fill(u64::MAX);
    }

    pub fn step(&mut self, bus: &mut impl SystemBus) -> StepResult {
        let mut ctx = ExecContext {
            regs: &mut self.regs,
            csr: &mut self.csr,
            mmu: &mut self.mmu,
            bus,
            priv_mode: &mut self.priv_mode,
            lr_addr: &mut self.lr_addr,
            fetch_vpage: &mut self.fetch_vpage,
            fetch_ppage: &mut self.fetch_ppage,
            fetch_satp: &mut self.fetch_satp,
            vregs: &mut self.vregs,

            #[cfg(not(target_arch = "wasm32"))]
            icache_tags: &mut self.icache_tags,

            #[cfg(not(target_arch = "wasm32"))]
            icache_data: &mut self.icache_data,
        };

        execute::step(&mut ctx)
    }

    pub fn run(&mut self, bus: &mut impl SystemBus, max_steps: u64) -> StepResult {
        let mut ctx = ExecContext {
            regs: &mut self.regs,
            csr: &mut self.csr,
            mmu: &mut self.mmu,
            bus,
            priv_mode: &mut self.priv_mode,
            lr_addr: &mut self.lr_addr,
            fetch_vpage: &mut self.fetch_vpage,
            fetch_ppage: &mut self.fetch_ppage,
            fetch_satp: &mut self.fetch_satp,
            vregs: &mut self.vregs,

            #[cfg(not(target_arch = "wasm32"))]
            icache_tags: &mut self.icache_tags,

            #[cfg(not(target_arch = "wasm32"))]
            icache_data: &mut self.icache_data,
        };

        execute::run(&mut ctx, max_steps)
    }
}
