use std::path::PathBuf;

use crate::clint::{CLINT_BASE, CLINT_SIZE, Clint, TIMER_FREQUENCY};
use crate::dtb;
use crate::plic::{PLIC_BASE, PLIC_SIZE, Plic};
use crate::uart::Uart;
use crate::virtio::RamView;
use crate::virtio::blk::VirtioBlk;
use crate::virtio::console::VirtioConsole;
use crate::virtio::fs::{Mount, VirtioFs};
use crate::virtio::net::VirtioNet;
use crate::virtio::slirp::SlirpBackend;

use crate::crypto_handler::CryptoHandler;
use crate::{
    GUEST_MAC, KERNEL_OFFSET, LOW_RAM_BASE, LOW_RAM_SIZE, RAM_BASE, UART_BASE, UART_CRYPTO_BASE,
    UART_CRYPTO_IRQ, UART_CRYPTO_SIZE, UART_CTRL_BASE, UART_CTRL_IRQ, UART_CTRL_SIZE,
    UART_DATA_BASE, UART_DATA_IRQ, UART_DATA_SIZE, UART_IRQ, UART_SIZE, UART_STDERR_BASE,
    UART_STDERR_IRQ, UART_STDERR_SIZE, VIRTIO_BASE, VIRTIO_BLK_IRQ, VIRTIO_CONSOLE_IRQ,
    VIRTIO_FS_BASE_IRQ, VIRTIO_MAX_FS, VIRTIO_NET_IRQ, VIRTIO_SIZE,
};

use riscv_core::csr::{MIP_MEIP, MIP_MSIP, MIP_MTIP, MIP_SEIP};
use riscv_core::{Hart, SystemBus};

pub struct MachineBus {
    pub ram: Vec<u8>,
    ram_mask: u64,
    pub low_ram: Vec<u8>,
    pub uart: Uart,
    pub uart_stderr: Uart,
    pub uart_ctrl: Uart,
    pub uart_data: Uart,
    pub uart_crypto: Uart,
    pub crypto_handler: CryptoHandler,
    pub clint: Clint,
    pub plic: Plic,
    pub blk: Option<VirtioBlk>,
    pub console: VirtioConsole,
    pub net: Option<VirtioNet<SlirpBackend>>,
    pub fs_devices: Vec<VirtioFs>,
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
            uart_stderr: Uart::new(),
            uart_ctrl: Uart::new(),
            uart_data: Uart::new(),
            uart_crypto: Uart::new(),
            crypto_handler: CryptoHandler::new(),
            clint: Clint::new(),
            plic: Plic::new(),
            blk: None,
            console: VirtioConsole::new(),
            net: None,
            fs_devices: Vec::new(),
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

    pub fn attach_fs(&mut self, mounts: Vec<Mount>) {
        self.fs_devices = (0..crate::VIRTIO_MAX_FS)
            .map(|i| {
                let tag = format!("vfs{}", i);
                if let Some(mount) = mounts.get(i).cloned() {
                    VirtioFs::new_single(mount, &tag)
                } else {
                    VirtioFs::new_single(
                        Mount {
                            host_path: PathBuf::new(),
                            tag: tag.clone(),
                            writable: false,
                        },
                        &tag,
                    )
                }
            })
            .collect();
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

        let (timer_interrupt_pending, software_interrupt_pending) =
            self.clint.get_interrupt_status();

        if timer_interrupt_pending {
            hart.csr.mip |= MIP_MTIP;
        } else {
            hart.csr.mip &= !MIP_MTIP;
        }

        if software_interrupt_pending {
            hart.csr.mip |= MIP_MSIP;
        } else {
            hart.csr.mip &= !MIP_MSIP;
        }

        self.plic.set_irq(UART_IRQ, self.uart.irq_pending.get());
        self.plic
            .set_irq(UART_STDERR_IRQ, self.uart_stderr.irq_pending.get());
        self.plic
            .set_irq(UART_CTRL_IRQ, self.uart_ctrl.irq_pending.get());
        self.plic
            .set_irq(UART_DATA_IRQ, self.uart_data.irq_pending.get());
        self.plic
            .set_irq(UART_CRYPTO_IRQ, self.uart_crypto.irq_pending.get());

        self.crypto_handler.process(&self.uart_crypto);

        if let Some(block_device) = &self.blk {
            self.plic
                .set_irq(VIRTIO_BLK_IRQ, block_device.mmio.int_status != 0);
        }

        self.plic
            .set_irq(VIRTIO_CONSOLE_IRQ, self.console.mmio.int_status != 0);

        if let Some(network_device) = &mut self.net {
            let mask = self.ram_mask;
            let mut ram = RamView::new(&mut self.ram, mask);
            network_device.poll_rx(&mut ram);
            self.plic
                .set_irq(VIRTIO_NET_IRQ, network_device.mmio.int_status != 0);
        }

        for (i, fs_device) in self.fs_devices.iter().enumerate() {
            self.plic.set_irq(
                VIRTIO_FS_BASE_IRQ + i as u32,
                fs_device.mmio.int_status != 0,
            );
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

    pub fn net_has_active_connections(&self) -> bool {
        self.net
            .as_ref()
            .is_some_and(|n| n.has_active_connections())
    }

    pub fn has_pending_io(&self) -> bool {
        self.uart.rx_pending() || self.uart_data.rx_pending() || self.net_rx_pending()
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
    fn ram_index(&self, physical_address: u64) -> usize {
        (physical_address - RAM_BASE) as usize & self.ram_mask as usize
    }

    #[inline(always)]
    fn ram_write_u8(&mut self, physical_address: u64, value: u8) {
        let index = self.ram_index(physical_address);
        unsafe {
            *self.ram.get_unchecked_mut(index) = value;
        }
    }

    #[inline(always)]
    fn ram_read_u8(&self, physical_address: u64) -> u8 {
        let index = self.ram_index(physical_address);

        unsafe { *self.ram.get_unchecked(index) }
    }

    fn virtio_device_slot(&self, address: u64) -> Option<usize> {
        let num_slots = 3 + VIRTIO_MAX_FS;
        if (VIRTIO_BASE..VIRTIO_BASE + VIRTIO_SIZE * num_slots as u64).contains(&address) {
            Some(((address - VIRTIO_BASE) / VIRTIO_SIZE) as usize)
        } else {
            None
        }
    }

    fn virtio_register_offset(address: u64) -> u64 {
        (address - VIRTIO_BASE) % VIRTIO_SIZE
    }
}

impl SystemBus for MachineBus {
    fn read_byte(&mut self, address: u64) -> u8 {
        if address >= RAM_BASE && address < RAM_BASE + self.ram_mask + 1 {
            return self.ram_read_u8(address);
        }

        if address < LOW_RAM_BASE + LOW_RAM_SIZE {
            return self.low_ram[(address - LOW_RAM_BASE) as usize];
        }

        if (UART_BASE..UART_BASE + UART_SIZE).contains(&address) {
            return self.uart.read_register((address - UART_BASE) as u8);
        }

        if (UART_STDERR_BASE..UART_STDERR_BASE + UART_STDERR_SIZE).contains(&address) {
            return self
                .uart_stderr
                .read_register((address - UART_STDERR_BASE) as u8);
        }

        if (UART_CTRL_BASE..UART_CTRL_BASE + UART_CTRL_SIZE).contains(&address) {
            return self
                .uart_ctrl
                .read_register((address - UART_CTRL_BASE) as u8);
        }

        if (UART_DATA_BASE..UART_DATA_BASE + UART_DATA_SIZE).contains(&address) {
            return self
                .uart_data
                .read_register((address - UART_DATA_BASE) as u8);
        }

        if (UART_CRYPTO_BASE..UART_CRYPTO_BASE + UART_CRYPTO_SIZE).contains(&address) {
            return self
                .uart_crypto
                .read_register((address - UART_CRYPTO_BASE) as u8);
        }

        if let Some(slot) = self.virtio_device_slot(address) {
            let offset = Self::virtio_register_offset(address);
            let word_offset = offset & !3;
            let byte_index = (offset & 3) as usize;
            let word = match slot {
                0 => self.blk.as_ref().map_or(0, |b| b.mmio.read(word_offset)),
                1 => self.console.mmio.read(word_offset),
                2 => self.net.as_ref().map_or(0, |n| n.mmio.read(word_offset)),
                s if s >= 3 => self
                    .fs_devices
                    .get(s - 3)
                    .map_or(0, |f| f.mmio.read(word_offset)),
                _ => 0,
            };

            return (word >> (byte_index * 8)) as u8;
        }

        0
    }

    fn read_halfword(&mut self, address: u64) -> u16 {
        u16::from_le_bytes([self.read_byte(address), self.read_byte(address + 1)])
    }

    fn read_word(&mut self, address: u64) -> u32 {
        if address >= RAM_BASE && address + 3 < RAM_BASE + self.ram.len() as u64 {
            let index = (address - RAM_BASE) as usize;
            return unsafe {
                u32::from_le_bytes(*(self.ram.as_ptr().add(index) as *const [u8; 4]))
            };
        }

        if (CLINT_BASE..CLINT_BASE + CLINT_SIZE - 3).contains(&address) {
            return self.clint.read_register(address - CLINT_BASE);
        }

        if (PLIC_BASE..PLIC_BASE + PLIC_SIZE - 3).contains(&address) {
            return self.plic.read_register(address - PLIC_BASE);
        }

        if let Some(slot) = self.virtio_device_slot(address) {
            let offset = Self::virtio_register_offset(address);
            return match slot {
                0 => self
                    .blk
                    .as_ref()
                    .map_or(0, |device| device.mmio.read(offset)),
                1 => self.console.mmio.read(offset),
                2 => self
                    .net
                    .as_ref()
                    .map_or(0, |device| device.mmio.read(offset)),
                s if s >= 3 => self
                    .fs_devices
                    .get(s - 3)
                    .map_or(0, |d| d.mmio.read(offset)),
                _ => 0,
            };
        }

        u32::from_le_bytes([
            self.read_byte(address),
            self.read_byte(address + 1),
            self.read_byte(address + 2),
            self.read_byte(address + 3),
        ])
    }

    fn read_doubleword(&mut self, address: u64) -> u64 {
        if address >= RAM_BASE && address + 7 < RAM_BASE + self.ram.len() as u64 {
            let index = (address - RAM_BASE) as usize;
            return unsafe {
                u64::from_le_bytes(*(self.ram.as_ptr().add(index) as *const [u8; 8]))
            };
        }

        (self.read_word(address) as u64) | ((self.read_word(address + 4) as u64) << 32)
    }

    fn write_byte(&mut self, address: u64, value: u8) {
        if address >= RAM_BASE && address < RAM_BASE + self.ram_mask + 1 {
            self.ram_write_u8(address, value);
            return;
        }

        if address < LOW_RAM_BASE + LOW_RAM_SIZE {
            self.low_ram[(address - LOW_RAM_BASE) as usize] = value;
            return;
        }

        if (UART_BASE..UART_BASE + UART_SIZE).contains(&address) {
            self.uart.write_register((address - UART_BASE) as u8, value);
            return;
        }

        if (UART_STDERR_BASE..UART_STDERR_BASE + UART_STDERR_SIZE).contains(&address) {
            self.uart_stderr
                .write_register((address - UART_STDERR_BASE) as u8, value);

            return;
        }

        if (UART_CTRL_BASE..UART_CTRL_BASE + UART_CTRL_SIZE).contains(&address) {
            self.uart_ctrl
                .write_register((address - UART_CTRL_BASE) as u8, value);

            return;
        }

        if (UART_DATA_BASE..UART_DATA_BASE + UART_DATA_SIZE).contains(&address) {
            self.uart_data
                .write_register((address - UART_DATA_BASE) as u8, value);
            return;
        }

        if (UART_CRYPTO_BASE..UART_CRYPTO_BASE + UART_CRYPTO_SIZE).contains(&address) {
            self.uart_crypto
                .write_register((address - UART_CRYPTO_BASE) as u8, value);
        }
    }

    fn write_halfword(&mut self, address: u64, value: u16) {
        let [low_byte, high_byte] = value.to_le_bytes();
        self.write_byte(address, low_byte);
        self.write_byte(address + 1, high_byte);
    }

    fn write_word(&mut self, address: u64, value: u32) {
        let [byte_0, byte_1, byte_2, byte_3] = value.to_le_bytes();

        if address >= RAM_BASE && address + 3 < RAM_BASE + self.ram.len() as u64 {
            let index = (address - RAM_BASE) as usize;
            unsafe {
                *(self.ram.as_mut_ptr().add(index) as *mut [u8; 4]) = value.to_le_bytes();
            }
            return;
        }

        if (CLINT_BASE..CLINT_BASE + CLINT_SIZE - 3).contains(&address) {
            self.clint.write_register(address - CLINT_BASE, value);
            return;
        }

        if (PLIC_BASE..PLIC_BASE + PLIC_SIZE - 3).contains(&address) {
            self.plic.write_register(address - PLIC_BASE, value);
            return;
        }

        if let Some(slot) = self.virtio_device_slot(address) {
            let offset = Self::virtio_register_offset(address);
            let notify_queue_index = match slot {
                0 => self
                    .blk
                    .as_mut()
                    .and_then(|device| device.mmio.write(offset, value)),
                1 => self.console.mmio.write(offset, value),
                2 => self
                    .net
                    .as_mut()
                    .and_then(|device| device.mmio.write(offset, value)),
                s if s >= 3 => self
                    .fs_devices
                    .get_mut(s - 3)
                    .and_then(|d| d.mmio.write(offset, value)),
                _ => None,
            };

            if let Some(queue_index) = notify_queue_index {
                let mask = self.ram_mask;
                let mut ram = RamView::new(&mut self.ram, mask);

                match slot {
                    0 => {
                        if let Some(block_device) = &mut self.blk {
                            block_device.notify(&mut ram);
                        }
                    }
                    1 => self.console.notify(queue_index, &mut ram),
                    2 => {
                        if let Some(network_device) = &mut self.net {
                            network_device.notify(queue_index, &mut ram);
                        }
                    }
                    s if s >= 3 => {
                        if let Some(fs_device) = self.fs_devices.get_mut(s - 3) {
                            fs_device.notify(queue_index, &mut ram);
                        }
                    }
                    _ => {}
                }
            }

            return;
        }

        self.write_byte(address, byte_0);
        self.write_byte(address + 1, byte_1);
        self.write_byte(address + 2, byte_2);
        self.write_byte(address + 3, byte_3);
    }

    fn write_doubleword(&mut self, address: u64, value: u64) {
        if address >= RAM_BASE && address + 7 < RAM_BASE + self.ram.len() as u64 {
            let index = (address - RAM_BASE) as usize;
            unsafe {
                *(self.ram.as_mut_ptr().add(index) as *mut [u8; 8]) = value.to_le_bytes();
            }
            return;
        }

        self.write_word(address, value as u32);
        self.write_word(address + 4, (value >> 32) as u32);
    }
}

fn kernel_entry_and_offset(kernel: &[u8]) -> (u64, u64) {
    if kernel.len() >= 64 && kernel[0..4] == *b"\x7fELF" {
        let entry_point = u64::from_le_bytes(kernel[24..32].try_into().unwrap());
        return (entry_point, entry_point.saturating_sub(RAM_BASE));
    }

    if kernel.len() >= 16 && kernel[0..2] == *b"MZ" {
        let text_offset = u64::from_le_bytes(kernel[8..16].try_into().unwrap());
        return (RAM_BASE + text_offset, text_offset);
    }

    (RAM_BASE + KERNEL_OFFSET, KERNEL_OFFSET)
}

pub fn boot(
    bus: &mut MachineBus,
    hart: &mut Hart,
    bios: Option<&[u8]>,
    kernel: &[u8],
    initrd: Option<&[u8]>,
    bootargs: &str,
) {
    let ram_size = bus.ram_size();

    let (entry_point, kernel_load_offset) = if bios.is_some() {
        (RAM_BASE, KERNEL_OFFSET)
    } else {
        kernel_entry_and_offset(kernel)
    };

    if let Some(bios_data) = bios {
        bus.load_ram(0, bios_data);
    }

    bus.load_ram(kernel_load_offset, kernel);

    let kernel_end_aligned = kernel_load_offset + ((kernel.len() as u64 + 0xfff) & !0xfff);

    let (initrd_start, initrd_end) = if let Some(initrd_data) = initrd {
        let after_kernel_aligned = (kernel_end_aligned + 0xff_ffff) & !0xf_ffff;

        let start_offset = if bios.is_some() {
            after_kernel_aligned.max(0x4000000)
        } else {
            after_kernel_aligned
        };

        assert!(
            start_offset + initrd_data.len() as u64 <= ram_size,
            "initrd too large for RAM: need {} MB, have {} MB",
            (start_offset + initrd_data.len() as u64) / (1024 * 1024),
            ram_size / (1024 * 1024),
        );

        bus.load_ram(start_offset, initrd_data);
        let end_offset = start_offset + initrd_data.len() as u64;

        (RAM_BASE + start_offset, RAM_BASE + end_offset)
    } else {
        (0, 0)
    };

    let dtb_data = dtb::build(
        RAM_BASE,
        ram_size,
        UART_BASE,
        CLINT_BASE,
        PLIC_BASE,
        UART_IRQ,
        TIMER_FREQUENCY,
        VIRTIO_BASE,
        VIRTIO_SIZE,
        &[VIRTIO_BLK_IRQ, VIRTIO_CONSOLE_IRQ, VIRTIO_NET_IRQ],
        bus.blk.is_some(),
        bus.net.is_some(),
        VIRTIO_MAX_FS,
        bootargs,
        initrd_start,
        initrd_end,
        UART_STDERR_BASE,
        UART_STDERR_IRQ,
        UART_CTRL_BASE,
        UART_CTRL_IRQ,
        UART_DATA_BASE,
        UART_DATA_IRQ,
        UART_CRYPTO_BASE,
        UART_CRYPTO_IRQ,
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

    let dtb_physical_address = RAM_BASE + dtb_offset;
    bus.load_ram(dtb_offset, &dtb_data);

    let _ = std::fs::write("/tmp/vpod.dtb", &dtb_data);

    let dtb_magic = u32::from_le_bytes(
        bus.ram[dtb_offset as usize..dtb_offset as usize + 4]
            .try_into()
            .unwrap(),
    );

    eprintln!(
        "[boot] entry={:#x} kernel={:#x}+{:#x} initrd={:#x}..{:#x} dtb={:#x} dtb_magic={:#010x}({}) dtb_size={}",
        entry_point,
        RAM_BASE + kernel_load_offset,
        kernel.len(),
        initrd_start,
        initrd_end,
        dtb_physical_address,
        dtb_magic.swap_bytes(),
        if dtb_magic.swap_bytes() == 0xd00dfeed {
            "OK"
        } else {
            "NOPE!"
        },
        dtb_data.len()
    );

    hart.regs.pc = entry_point;
    hart.regs.write(10, 0);
    hart.regs.write(11, dtb_physical_address);
}
