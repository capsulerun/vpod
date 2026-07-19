use crate::block::BlockCache;
use crate::csr::{Csr, PrivMode};
use crate::execute::{self, ExecContext, FetchTlb};

use crate::execute::ICACHE_SIZE;

use crate::gpr::Gpr;
use crate::mmu::Mmu;
use crate::system_bus::SystemBus;
use crate::trap::StepResult;

pub struct Hart {
    pub regs: Gpr,
    pub csr: Csr,
    pub mmu: Mmu,
    pub priv_mode: PrivMode,
    pub lr_addr: Option<u64>,
    pub fetch_tlb: FetchTlb,

    pub icache_tags: Box<[u64; ICACHE_SIZE]>,
    pub icache_data: Box<[u32; ICACHE_SIZE]>,
    pub is_waiting: bool,
    pub blocks: BlockCache,
}

impl Hart {
    pub fn new(entry: u64) -> Self {
        Self {
            regs: Gpr::new(entry),
            csr: Csr::new(),
            mmu: Mmu::new(),
            priv_mode: PrivMode::M,
            lr_addr: None,
            fetch_tlb: FetchTlb::new(),

            icache_tags: Box::new([u64::MAX; ICACHE_SIZE]),
            icache_data: Box::new([0u32; ICACHE_SIZE]),
            is_waiting: false,
            blocks: BlockCache::new(),
        }
    }

    pub fn invalidate_icache(&mut self) {
        self.icache_tags.fill(u64::MAX);
        self.blocks.flush_all();
    }

    pub fn step(&mut self, bus: &mut impl SystemBus) -> StepResult {
        let mut ctx = ExecContext {
            regs: &mut self.regs,
            csr: &mut self.csr,
            mmu: &mut self.mmu,
            bus,
            priv_mode: &mut self.priv_mode,
            lr_addr: &mut self.lr_addr,
            fetch_tlb: &mut self.fetch_tlb,

            icache_tags: &mut self.icache_tags,
            icache_data: &mut self.icache_data,

            is_waiting: &mut self.is_waiting,
            blocks: &mut self.blocks,
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
            fetch_tlb: &mut self.fetch_tlb,
            icache_tags: &mut self.icache_tags,
            icache_data: &mut self.icache_data,
            is_waiting: &mut self.is_waiting,
            blocks: &mut self.blocks,
        };

        execute::run(&mut ctx, max_steps)
    }

    pub fn run_until_wait(&mut self, bus: &mut impl SystemBus, max_steps: u64) -> StepResult {
        let mut ctx = ExecContext {
            regs: &mut self.regs,
            csr: &mut self.csr,
            mmu: &mut self.mmu,
            bus,
            priv_mode: &mut self.priv_mode,
            lr_addr: &mut self.lr_addr,
            fetch_tlb: &mut self.fetch_tlb,
            icache_tags: &mut self.icache_tags,
            icache_data: &mut self.icache_data,
            is_waiting: &mut self.is_waiting,
            blocks: &mut self.blocks,
        };

        execute::run_until_wait(&mut ctx, max_steps)
    }
}
