use crate::system_bus::SystemBus;
use crate::perf;

const PTE_V: u64 = 1 << 0;
const PTE_R: u64 = 1 << 1;
const PTE_W: u64 = 1 << 2;
const PTE_X: u64 = 1 << 3;
const PTE_D: u64 = 1 << 7;

const TLB_SIZE: usize = 512;
const TLB_MASK: u64 = (TLB_SIZE - 1) as u64;

#[derive(Clone, Copy)]
struct TlbEntry {
    virt_page_num: u64,
    phys_page_num: u64,
    flags: u64,
    epoch: u32,
}

impl TlbEntry {
    const EMPTY: Self = Self {
        virt_page_num: u64::MAX,
        phys_page_num: 0,
        flags: 0,
        epoch: 0,
    };
}

pub struct Mmu {
    tlb: [TlbEntry; TLB_SIZE],
    epoch: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmuFault {
    LoadPageFault(u64),
    StorePageFault(u64),
    InstructionPageFault(u64),
    LoadAccessFault(u64),
    StoreAccessFault(u64),
    InstructionAccessFault(u64),
}

impl MmuFault {
    pub fn mcause(&self) -> u64 {
        match self {
            Self::InstructionAccessFault(_) => 1,
            Self::LoadAccessFault(_) => 5,
            Self::StoreAccessFault(_) => 7,
            Self::InstructionPageFault(_) => 12,
            Self::LoadPageFault(_) => 13,
            Self::StorePageFault(_) => 15,
        }
    }
    pub fn tval(&self) -> u64 {
        match self {
            Self::LoadPageFault(address)
            | Self::StorePageFault(address)
            | Self::InstructionPageFault(address)
            | Self::LoadAccessFault(address)
            | Self::StoreAccessFault(address)
            | Self::InstructionAccessFault(address) => *address,
        }
    }
}

impl Default for Mmu {
    fn default() -> Self {
        Self::new()
    }
}

impl Mmu {
    pub fn new() -> Self {
        Self {
            tlb: [TlbEntry::EMPTY; TLB_SIZE],
            epoch: 1,
        }
    }

    pub fn flush(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.epoch = 1;
        }
    }

    #[inline(always)]
    fn lookup(&self, virt_page_num: u64) -> Option<(u64, u64)> {
        let slot = (virt_page_num & TLB_MASK) as usize;
        let entry = &self.tlb[slot];

        if entry.epoch == self.epoch && entry.virt_page_num == virt_page_num {
            Some((entry.phys_page_num, entry.flags))
        } else {
            None
        }
    }

    #[inline(always)]
    fn insert(&mut self, virt_page_num: u64, phys_page_num: u64, flags: u64) {
        let slot = (virt_page_num & TLB_MASK) as usize;
        self.tlb[slot] = TlbEntry {
            virt_page_num,
            phys_page_num,
            flags,
            epoch: self.epoch,
        };
    }

    pub fn translate_fetch(
        &mut self,
        virtual_address: u64,
        satp: u64,
        bus: &mut impl SystemBus,
    ) -> Result<u64, MmuFault> {
        if satp >> 60 == 0 {
            perf::note_bare_translate();
            return Ok(virtual_address);
        }

        let vpn = virtual_address >> 12;
        if let Some((ppn, flags)) = self.lookup(vpn)
            && flags & PTE_X != 0
        {
            perf::note_tlb_hit();
            return Ok((ppn << 12) | (virtual_address & 0xfff));
        }

        perf::note_tlb_walk();
        self.walk(virtual_address, satp, false, true, bus)
            .map_err(|_| MmuFault::InstructionPageFault(virtual_address))
    }

    pub fn translate_load(
        &mut self,
        virtual_address: u64,
        satp: u64,
        bus: &mut impl SystemBus,
    ) -> Result<u64, MmuFault> {
        if satp >> 60 == 0 {
            perf::note_bare_translate();
            return Ok(virtual_address);
        }

        let vpn = virtual_address >> 12;
        if let Some((ppn, flags)) = self.lookup(vpn)
            && flags & PTE_R != 0
        {
            perf::note_tlb_hit();
            return Ok((ppn << 12) | (virtual_address & 0xfff));
        }

        perf::note_tlb_walk();
        self.walk(virtual_address, satp, false, false, bus)
            .map_err(|_| MmuFault::LoadPageFault(virtual_address))
    }

    pub fn translate_store(
        &mut self,
        virtual_address: u64,
        satp: u64,
        bus: &mut impl SystemBus,
    ) -> Result<u64, MmuFault> {
        if satp >> 60 == 0 {
            perf::note_bare_translate();
            return Ok(virtual_address);
        }

        let vpn = virtual_address >> 12;
        if let Some((ppn, flags)) = self.lookup(vpn)
            && flags & (PTE_W | PTE_D) == (PTE_W | PTE_D)
        {
            perf::note_tlb_hit();
            return Ok((ppn << 12) | (virtual_address & 0xfff));
        }

        perf::note_tlb_walk();
        self.walk(virtual_address, satp, true, false, bus)
            .map_err(|_| MmuFault::StorePageFault(virtual_address))
    }

    fn walk(
        &mut self,
        virtual_address: u64,
        satp: u64,
        write: bool,
        exec: bool,
        bus: &mut impl SystemBus,
    ) -> Result<u64, ()> {
        let (physical_address, vpn, pte) = walk_inner(virtual_address, satp, write, exec, bus)?;
        self.insert(vpn, physical_address >> 12, pte);

        Ok(physical_address)
    }
}

fn walk_inner(
    virtual_address: u64,
    satp: u64,
    write: bool,
    exec: bool,
    bus: &mut impl SystemBus,
) -> Result<(u64, u64, u64), ()> {
    let root_ppn = satp & 0x0fff_ffff_ffff;
    let virt_page_nums = [
        (virtual_address >> 30) & 0x1ff,
        (virtual_address >> 21) & 0x1ff,
        (virtual_address >> 12) & 0x1ff,
    ];

    let mut phys_page_num = root_ppn;
    let mut pte: u64 = 0;
    let mut level = 2i32;

    while level >= 0 {
        let pte_addr = (phys_page_num << 12) | (virt_page_nums[2 - level as usize] << 3);
        pte = bus.read_doubleword(pte_addr);
        if pte & PTE_V == 0 {
            return Err(());
        }

        if pte & (PTE_R | PTE_X) != 0 {
            break;
        }

        phys_page_num = (pte >> 10) & 0x0fff_ffff_ffff;
        level -= 1;
    }

    if level < 0 {
        return Err(());
    }

    if exec && pte & PTE_X == 0 {
        return Err(());
    }
    if !exec && !write && pte & PTE_R == 0 {
        return Err(());
    }
    if write && pte & PTE_W == 0 {
        return Err(());
    }

    let leaf_ppn = (pte >> 10) & 0x0fff_ffff_ffff;

    if level > 0 {
        let align_mask = (1u64 << (9 * level as u32)) - 1;

        if leaf_ppn & align_mask != 0 {
            return Err(());
        }
    }

    let page_offset_bits = 12 + 9 * level as u32;
    let page_offset_mask = (1u64 << page_offset_bits) - 1;
    let physical_address = (leaf_ppn << 12) | (virtual_address & page_offset_mask);
    let vpn = virtual_address >> 12;

    Ok((physical_address, vpn, pte))
}
