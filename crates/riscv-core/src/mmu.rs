use crate::perf;
use crate::system_bus::SystemBus;

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

#[derive(Clone, Copy)]
struct LoadFastEntry {
    virt_page_num: u64,
    satp: u64,
    ram_epoch: u64,
    epoch: u32,
    host_page: *const u8,
}

impl LoadFastEntry {
    const EMPTY: Self = Self {
        virt_page_num: u64::MAX,
        satp: 0,
        ram_epoch: 0,
        epoch: 0,
        host_page: std::ptr::null(),
    };
}

unsafe impl Send for LoadFastEntry {}
unsafe impl Sync for LoadFastEntry {}

#[derive(Clone, Copy)]
struct StoreFastEntry {
    virt_page_num: u64,
    satp: u64,
    ram_epoch: u64,
    code_generation: u64,
    epoch: u32,
    host_page: *mut u8,
}

impl StoreFastEntry {
    const EMPTY: Self = Self {
        virt_page_num: u64::MAX,
        satp: 0,
        ram_epoch: 0,
        code_generation: 0,
        epoch: 0,
        host_page: std::ptr::null_mut(),
    };
}

unsafe impl Send for StoreFastEntry {}
unsafe impl Sync for StoreFastEntry {}

pub struct Mmu {
    tlb: [TlbEntry; TLB_SIZE],
    load_fast: [LoadFastEntry; TLB_SIZE],
    store_fast: [StoreFastEntry; TLB_SIZE],
    epoch: u32,

    load_vpage: u64,
    load_ppage: u64,
    load_satp: u64,
    store_vpage: u64,
    store_ppage: u64,
    store_satp: u64,
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
            load_fast: [LoadFastEntry::EMPTY; TLB_SIZE],
            store_fast: [StoreFastEntry::EMPTY; TLB_SIZE],
            epoch: 1,
            load_vpage: u64::MAX,
            load_ppage: 0,
            load_satp: u64::MAX,
            store_vpage: u64::MAX,
            store_ppage: 0,
            store_satp: u64::MAX,
        }
    }

    pub fn flush(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.epoch = 1;
        }
        self.load_vpage = u64::MAX;
        self.store_vpage = u64::MAX;
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
    pub fn load_fast_lookup(
        &self,
        virtual_address: u64,
        satp: u64,
        ram_epoch: u64,
    ) -> Option<*const u8> {
        let virt_page_num = virtual_address >> 12;
        let entry = &self.load_fast[(virt_page_num & TLB_MASK) as usize];

        if entry.virt_page_num == virt_page_num
            && entry.satp == satp
            && entry.epoch == self.epoch
            && entry.ram_epoch == ram_epoch
        {
            Some(entry.host_page)
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn store_fast_lookup(
        &self,
        virtual_address: u64,
        satp: u64,
        ram_epoch: u64,
        code_generation: u64,
    ) -> Option<*mut u8> {
        let virt_page_num = virtual_address >> 12;
        let entry = &self.store_fast[(virt_page_num & TLB_MASK) as usize];

        if entry.virt_page_num == virt_page_num
            && entry.satp == satp
            && entry.epoch == self.epoch
            && entry.ram_epoch == ram_epoch
            && entry.code_generation == code_generation
        {
            Some(entry.host_page)
        } else {
            None
        }
    }

    #[inline]
    pub fn store_fast_fill(
        &mut self,
        virt_page_num: u64,
        satp: u64,
        physical_address: u64,
        code_generation: u64,
        bus: &mut impl SystemBus,
    ) {
        if let Some(host_page) = bus.ram_store_page(physical_address) {
            self.store_fast[(virt_page_num & TLB_MASK) as usize] = StoreFastEntry {
                virt_page_num,
                satp,
                ram_epoch: bus.ram_epoch(),
                code_generation,
                epoch: self.epoch,
                host_page,
            };
        }
    }

    #[inline]
    fn load_fast_fill(
        &mut self,
        virt_page_num: u64,
        satp: u64,
        physical_address: u64,
        bus: &mut impl SystemBus,
    ) {
        if let Some(host_page) = bus.ram_load_page(physical_address) {
            self.load_fast[(virt_page_num & TLB_MASK) as usize] = LoadFastEntry {
                virt_page_num,
                satp,
                ram_epoch: bus.ram_epoch(),
                epoch: self.epoch,
                host_page,
            };
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
        let vpn = virtual_address >> 12;

        if satp >> 60 == 0 {
            perf::note_bare_translate();
            self.load_fast_fill(vpn, satp, virtual_address, bus);
            return Ok(virtual_address);
        }

        if vpn == self.load_vpage && satp == self.load_satp {
            perf::note_tlb_hit();
            let pa = (self.load_ppage << 12) | (virtual_address & 0xfff);
            self.load_fast_fill(vpn, satp, pa, bus);
            return Ok(pa);
        }

        if let Some((ppn, flags)) = self.lookup(vpn)
            && flags & PTE_R != 0
        {
            perf::note_tlb_hit();
            self.load_vpage = vpn;
            self.load_ppage = ppn;
            self.load_satp = satp;
            let pa = (ppn << 12) | (virtual_address & 0xfff);
            self.load_fast_fill(vpn, satp, pa, bus);
            return Ok(pa);
        }

        perf::note_tlb_walk();
        let pa = self
            .walk(virtual_address, satp, false, false, bus)
            .map_err(|_| MmuFault::LoadPageFault(virtual_address))?;
        self.load_vpage = vpn;
        self.load_ppage = pa >> 12;
        self.load_satp = satp;
        self.load_fast_fill(vpn, satp, pa, bus);
        Ok(pa)
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
        if vpn == self.store_vpage && satp == self.store_satp {
            perf::note_tlb_hit();
            return Ok((self.store_ppage << 12) | (virtual_address & 0xfff));
        }

        if let Some((ppn, flags)) = self.lookup(vpn)
            && flags & (PTE_W | PTE_D) == (PTE_W | PTE_D)
        {
            perf::note_tlb_hit();
            self.store_vpage = vpn;
            self.store_ppage = ppn;
            self.store_satp = satp;
            return Ok((ppn << 12) | (virtual_address & 0xfff));
        }

        perf::note_tlb_walk();
        let pa = self
            .walk(virtual_address, satp, true, false, bus)
            .map_err(|_| MmuFault::StorePageFault(virtual_address))?;
        self.store_vpage = vpn;
        self.store_ppage = pa >> 12;
        self.store_satp = satp;
        Ok(pa)
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
