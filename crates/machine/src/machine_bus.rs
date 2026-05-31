use crate::clint::{CLINT_BASE, CLINT_SIZE, Clint, RTC_FREQ};
use crate::dtb;
use crate::plic::{PLIC_BASE, PLIC_SIZE, Plic};
use crate::uart::Uart;
use crate::virtio::RamView;
use crate::virtio::blk::VirtioBlk;
use crate::virtio::console::VirtioConsole;
use crate::virtio::net::VirtioNet;
use crate::virtio::slirp::SlirpBackend;

use crate::{
    GUEST_MAC, KERNEL_OFFSET, LOW_RAM_BASE, LOW_RAM_SIZE, RAM_BASE, UART_BASE, UART_IRQ, UART_SIZE,
    VIRTIO_BASE, VIRTIO_BLK_IRQ, VIRTIO_CONSOLE_IRQ, VIRTIO_NET_IRQ, VIRTIO_SIZE,
};

use riscv_core::csr::{MIP_MEIP, MIP_MSIP, MIP_MTIP, MIP_SEIP};
use riscv_core::{Hart, SystemBus};
pub struct MachineBus {
    pub ram: Vec<u8>,
    ram_mask: u64,
    pub low_ram: Vec<u8>,
    pub uart: Uart,
    pub clint: Clint,
    pub plic: Plic,
    pub blk: Option<VirtioBlk>,
    pub console: VirtioConsole,
    pub net: Option<VirtioNet<SlirpBackend>>,
}

impl MachineBus {
    pub fn new(ram_size: u64) -> Self {
        assert!(
            ram_size.is_power_of_two(),
            "ram_size must be a power of two"
        );
        Self {
            ram: vec![0u8; ram_size as usize + 8],
            ram_mask: ram_size - 1,
            low_ram: vec![0u8; LOW_RAM_SIZE as usize],
            uart: Uart::new(),
            clint: Clint::new(),
            plic: Plic::new(),
            blk: None,
            console: VirtioConsole::new(),
            net: None,
        }
    }

    pub fn attach_blk(&mut self, file: std::fs::File) -> std::io::Result<()> {
        self.blk = Some(VirtioBlk::new(file)?);
        Ok(())
    }

    pub fn attach_net(&mut self) {
        let backend = SlirpBackend::new(GUEST_MAC);
        self.net = Some(VirtioNet::new(backend, GUEST_MAC));
    }

    pub fn ram_size(&self) -> u64 {
        self.ram.len() as u64
    }

    pub fn load_ram(&mut self, offset: u64, data: &[u8]) {
        let start = offset as usize;
        self.ram[start..start + data.len()].copy_from_slice(data);
    }

    pub fn load_low_ram(&mut self, offset: u64, data: &[u8]) {
        let start = offset as usize;
        self.low_ram[start..start + data.len()].copy_from_slice(data);
    }

    pub fn poll(&mut self, hart: &mut Hart) {
        hart.csr.time = self.clint.mtime();
        let (mtip, msip) = self.clint.tick();

        if mtip {
            hart.csr.mip |= MIP_MTIP;
        } else {
            hart.csr.mip &= !MIP_MTIP;
        }

        if msip {
            hart.csr.mip |= MIP_MSIP;
        } else {
            hart.csr.mip &= !MIP_MSIP;
        }

        self.plic.set_irq(UART_IRQ, self.uart.irq_pending.get());

        if let Some(blk) = &self.blk {
            self.plic.set_irq(VIRTIO_BLK_IRQ, blk.mmio.int_status != 0);
        }
        self.plic
            .set_irq(VIRTIO_CONSOLE_IRQ, self.console.mmio.int_status != 0);

        if let Some(net) = &mut self.net {
            let mask = self.ram_mask;
            let mut ram = RamView::new(&mut self.ram, mask);
            net.poll_rx(&mut ram);
            self.plic.set_irq(VIRTIO_NET_IRQ, net.mmio.int_status != 0);
        }

        if self.plic.irq_pending() {
            hart.csr.mip |= MIP_MEIP | MIP_SEIP;
        } else {
            hart.csr.mip &= !(MIP_MEIP | MIP_SEIP);
        }
    }

    pub fn net_rx_pending(&self) -> bool {
        self.net.as_ref().is_some_and(|n| n.rx_pending())
    }

    pub fn has_pending_io(&self) -> bool {
        self.uart.rx_pending() || self.net_rx_pending()
    }


    pub fn drain_console_tx(&mut self) -> Vec<u8> {
        let bytes = self.console.tx_buf.clone();
        self.console.tx_buf.clear();
        bytes
    }

    pub fn flush_console_to_stdout(&mut self) {
        self.console.flush_tx_to_stdout();
    }

    pub fn flush_console_rx(&mut self) {
        let mask = self.ram_mask;
        let mut ram = RamView::new(&mut self.ram, mask);
        self.console.flush_rx(&mut ram);
    }

    #[inline(always)]
    fn ram_idx(&self, pa: u64) -> usize {
        (pa - RAM_BASE) as usize & self.ram_mask as usize
    }

    #[inline(always)]
    fn ram_write_u8(&mut self, pa: u64, val: u8) {
        let idx = self.ram_idx(pa);
        unsafe {
            *self.ram.get_unchecked_mut(idx) = val;
        }
    }

    #[inline(always)]
    fn ram_read_u8(&self, pa: u64) -> u8 {
        let idx = self.ram_idx(pa);
        unsafe { *self.ram.get_unchecked(idx) }
    }

    fn virtio_slot(addr: u64) -> Option<usize> {
        if (VIRTIO_BASE..VIRTIO_BASE + VIRTIO_SIZE * 3).contains(&addr) {
            Some(((addr - VIRTIO_BASE) / VIRTIO_SIZE) as usize)
        } else {
            None
        }
    }

    fn virtio_offset(addr: u64) -> u64 {
        (addr - VIRTIO_BASE) % VIRTIO_SIZE
    }
}

impl SystemBus for MachineBus {
    fn read_byte(&mut self, addr: u64) -> u8 {
        if addr >= RAM_BASE && addr < RAM_BASE + self.ram_mask + 1 {
            return self.ram_read_u8(addr);
        }

        if addr < LOW_RAM_BASE + LOW_RAM_SIZE {
            return self.low_ram[(addr - LOW_RAM_BASE) as usize];
        }

        if (UART_BASE..UART_BASE + UART_SIZE).contains(&addr) {
            return self.uart.read((addr - UART_BASE) as u8);
        }

        if let Some(slot) = Self::virtio_slot(addr) {
            let off = Self::virtio_offset(addr);
            let word_off = off & !3;
            let byte_idx = (off & 3) as usize;
            let word = match slot {
                0 => self.blk.as_ref().map_or(0, |b| b.mmio.read(word_off)),
                1 => self.console.mmio.read(word_off),
                2 => self.net.as_ref().map_or(0, |n| n.mmio.read(word_off)),
                _ => 0,
            };
            return (word >> (byte_idx * 8)) as u8;
        }

        0
    }

    fn read_halfword(&mut self, addr: u64) -> u16 {
        u16::from_le_bytes([self.read_byte(addr), self.read_byte(addr + 1)])
    }

    fn read_word(&mut self, addr: u64) -> u32 {
        if addr >= RAM_BASE && addr + 3 < RAM_BASE + self.ram.len() as u64 {
            let i = (addr - RAM_BASE) as usize;
            return unsafe { u32::from_le_bytes(*(self.ram.as_ptr().add(i) as *const [u8; 4])) };
        }

        if (CLINT_BASE..CLINT_BASE + CLINT_SIZE - 3).contains(&addr) {
            return self.clint.read(addr - CLINT_BASE);
        }

        if (PLIC_BASE..PLIC_BASE + PLIC_SIZE - 3).contains(&addr) {
            return self.plic.read(addr - PLIC_BASE);
        }

        if let Some(slot) = Self::virtio_slot(addr) {
            let off = Self::virtio_offset(addr);
            return match slot {
                0 => self.blk.as_ref().map_or(0, |b| b.mmio.read(off)),
                1 => self.console.mmio.read(off),
                2 => self.net.as_ref().map_or(0, |n| n.mmio.read(off)),
                _ => 0,
            };
        }

        u32::from_le_bytes([
            self.read_byte(addr),
            self.read_byte(addr + 1),
            self.read_byte(addr + 2),
            self.read_byte(addr + 3),
        ])
    }

    fn read_doubleword(&mut self, addr: u64) -> u64 {
        if addr >= RAM_BASE && addr + 7 < RAM_BASE + self.ram.len() as u64 {
            let i = (addr - RAM_BASE) as usize;
            return unsafe { u64::from_le_bytes(*(self.ram.as_ptr().add(i) as *const [u8; 8])) };
        }

        (self.read_word(addr) as u64) | ((self.read_word(addr + 4) as u64) << 32)
    }

    fn write_byte(&mut self, addr: u64, val: u8) {
        if addr >= RAM_BASE && addr < RAM_BASE + self.ram_mask + 1 {
            self.ram_write_u8(addr, val);
            return;
        }

        if addr < LOW_RAM_BASE + LOW_RAM_SIZE {
            self.low_ram[(addr - LOW_RAM_BASE) as usize] = val;
            return;
        }

        if (UART_BASE..UART_BASE + UART_SIZE).contains(&addr) {
            self.uart.write((addr - UART_BASE) as u8, val);
        }
    }

    fn write_halfword(&mut self, addr: u64, val: u16) {
        let [lo, hi] = val.to_le_bytes();
        self.write_byte(addr, lo);
        self.write_byte(addr + 1, hi);
    }

    fn write_word(&mut self, addr: u64, val: u32) {
        let [a, b, c, d] = val.to_le_bytes();

        if addr >= RAM_BASE && addr + 3 < RAM_BASE + self.ram.len() as u64 {
            let i = (addr - RAM_BASE) as usize;
            unsafe {
                *(self.ram.as_mut_ptr().add(i) as *mut [u8; 4]) = val.to_le_bytes();
            }
            return;
        }

        if (CLINT_BASE..CLINT_BASE + CLINT_SIZE - 3).contains(&addr) {
            self.clint.write(addr - CLINT_BASE, val);
            return;
        }

        if (PLIC_BASE..PLIC_BASE + PLIC_SIZE - 3).contains(&addr) {
            self.plic.write(addr - PLIC_BASE, val);
            return;
        }

        if let Some(slot) = Self::virtio_slot(addr) {
            let off = Self::virtio_offset(addr);
            let notify = match slot {
                0 => self.blk.as_mut().and_then(|b| b.mmio.write(off, val)),
                1 => self.console.mmio.write(off, val),
                2 => self.net.as_mut().and_then(|n| n.mmio.write(off, val)),
                _ => None,
            };

            if let Some(queue_idx) = notify {
                let mask = self.ram_mask;
                let mut ram = RamView::new(&mut self.ram, mask);
                match slot {
                    0 => {
                        if let Some(blk) = &mut self.blk {
                            blk.notify(&mut ram);
                        }
                    }
                    1 => self.console.notify(queue_idx, &mut ram),
                    2 => {
                        if let Some(net) = &mut self.net {
                            net.notify(queue_idx, &mut ram);
                        }
                    }
                    _ => {}
                }
            }

            return;
        }

        self.write_byte(addr, a);
        self.write_byte(addr + 1, b);
        self.write_byte(addr + 2, c);
        self.write_byte(addr + 3, d);
    }

    fn write_doubleword(&mut self, addr: u64, val: u64) {
        if addr >= RAM_BASE && addr + 7 < RAM_BASE + self.ram.len() as u64 {
            let i = (addr - RAM_BASE) as usize;
            unsafe {
                *(self.ram.as_mut_ptr().add(i) as *mut [u8; 8]) = val.to_le_bytes();
            }
            return;
        }

        self.write_word(addr, val as u32);
        self.write_word(addr + 4, (val >> 32) as u32);
    }
}

fn kernel_entry_and_offset(kernel: &[u8]) -> (u64, u64) {
    if kernel.len() >= 64 && kernel[0..4] == *b"\x7fELF" {
        let e_entry = u64::from_le_bytes(kernel[24..32].try_into().unwrap());
        return (e_entry, e_entry.saturating_sub(RAM_BASE));
    }

    if kernel.len() >= 16 && kernel[0..2] == *b"MZ" {
        let text_offset = u64::from_le_bytes(kernel[8..16].try_into().unwrap());
        return (RAM_BASE + text_offset, text_offset);
    }

    (RAM_BASE + KERNEL_OFFSET, KERNEL_OFFSET)
}

pub fn boot(bus: &mut MachineBus, hart: &mut Hart, kernel: &[u8], bootargs: &str) {
    boot_with_bios(bus, hart, None, kernel, None, bootargs);
}

pub fn boot_with_bios(
    bus: &mut MachineBus,
    hart: &mut Hart,
    bios: Option<&[u8]>,
    kernel: &[u8],
    initrd: Option<&[u8]>,
    bootargs: &str,
) {
    let ram_size = bus.ram_size();

    let (entry, kernel_load_offset) = if bios.is_some() {
        (RAM_BASE, KERNEL_OFFSET)
    } else {
        kernel_entry_and_offset(kernel)
    };

    if let Some(bios_data) = bios {
        bus.load_ram(0, bios_data);
    }
    bus.load_ram(kernel_load_offset, kernel);

    let _kernel_end = kernel_load_offset + ((kernel.len() as u64 + 0xfff) & !0xfff);

    let (initrd_start, initrd_end) = if let Some(rd) = initrd {
        let after_kernel = (_kernel_end + 0xff_ffff) & !0xf_ffff;
        let start_offset = if bios.is_some() {
            after_kernel.max(0x4000000) // to be past DTB at 0x2200000
        } else {
            after_kernel
        };

        assert!(
            start_offset + rd.len() as u64 <= ram_size,
            "initrd too large for RAM: need {} MB, have {} MB",
            (start_offset + rd.len() as u64) / (1024 * 1024),
            ram_size / (1024 * 1024),
        );
        bus.load_ram(start_offset, rd);
        let end_offset = start_offset + rd.len() as u64;

        (RAM_BASE + start_offset, RAM_BASE + end_offset)
    } else {
        (0, 0)
    };

    let _initrd_size = if initrd_start != 0 {
        initrd_end - initrd_start
    } else {
        0
    };

    let dtb_data = dtb::build(
        RAM_BASE,
        ram_size,
        UART_BASE,
        CLINT_BASE,
        PLIC_BASE,
        UART_IRQ,
        RTC_FREQ,
        VIRTIO_BASE,
        VIRTIO_SIZE,
        &[VIRTIO_BLK_IRQ, VIRTIO_CONSOLE_IRQ, VIRTIO_NET_IRQ],
        bus.blk.is_some(),
        bus.net.is_some(),
        bootargs,
        initrd_start,
        initrd_end,
    );

    let dtb_offset = if bios.is_some() {
        0x2200000u64
    } else {
        (ram_size - dtb_data.len() as u64) & !0xfff
    };

    assert!(
        dtb_offset + dtb_data.len() as u64 <= ram_size,
        "DTB offset {:#x} out of range for {}MB RAM",
        dtb_offset,
        ram_size >> 20
    );

    let dtb_phys = RAM_BASE + dtb_offset;
    bus.load_ram(dtb_offset, &dtb_data);

    let _ = std::fs::write("/tmp/capsulev.dtb", &dtb_data);
    let dtb_magic = u32::from_le_bytes(
        bus.ram[dtb_offset as usize..dtb_offset as usize + 4]
            .try_into()
            .unwrap(),
    );

    eprintln!(
        "[boot] entry={:#x} kernel={:#x}+{:#x} initrd={:#x}..{:#x} dtb={:#x} dtb_magic={:#010x}({}) dtb_size={}",
        entry,
        RAM_BASE + kernel_load_offset,
        kernel.len(),
        initrd_start,
        initrd_end,
        dtb_phys,
        dtb_magic.swap_bytes(),
        if dtb_magic.swap_bytes() == 0xd00dfeed {
            "OK"
        } else {
            "NOPE!"
        },
        dtb_data.len()
    );

    hart.regs.pc = entry;
    hart.regs.write(10, 0);
    hart.regs.write(11, dtb_phys);
}
