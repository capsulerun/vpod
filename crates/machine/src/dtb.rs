const FDT_MAGIC: u32 = 0xd00dfeed;
const FDT_BEGIN_NODE: u32 = 0x00000001;
const FDT_END_NODE: u32 = 0x00000002;
const FDT_PROP: u32 = 0x00000003;
const FDT_END: u32 = 0x00000009;
const FDT_VERSION: u32 = 17;

pub struct DtbBuilder {
    structure: Vec<u8>,
    strings: Vec<u8>,
}

impl Default for DtbBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl DtbBuilder {
    pub fn new() -> Self {
        Self {
            structure: Vec::new(),
            strings: Vec::new(),
        }
    }

    pub fn begin_node(&mut self, name: &str) {
        self.push_u32(FDT_BEGIN_NODE);
        self.structure.extend_from_slice(name.as_bytes());
        self.structure.push(0);
        self.align4();
    }

    pub fn end_node(&mut self) {
        self.push_u32(FDT_END_NODE);
    }

    pub fn prop_u32(&mut self, name: &str, val: u32) {
        let off = self.string_offset(name);

        self.push_u32(FDT_PROP);
        self.push_u32(4);
        self.push_u32(off);
        self.push_u32(val);
    }

    pub fn prop_u64(&mut self, name: &str, val: u64) {
        let off = self.string_offset(name);

        self.push_u32(FDT_PROP);
        self.push_u32(8);
        self.push_u32(off);
        self.push_u32((val >> 32) as u32);
        self.push_u32(val as u32);
    }

    pub fn prop_str(&mut self, name: &str, val: &str) {
        let off = self.string_offset(name);

        self.push_u32(FDT_PROP);
        self.push_u32((val.len() + 1) as u32);
        self.push_u32(off);
        self.structure.extend_from_slice(val.as_bytes());
        self.structure.push(0);
        self.align4();
    }

    pub fn prop_reg(&mut self, addr: u64, size: u64) {
        let off = self.string_offset("reg");

        self.push_u32(FDT_PROP);
        self.push_u32(16);
        self.push_u32(off);
        self.push_u32((addr >> 32) as u32);
        self.push_u32(addr as u32);
        self.push_u32((size >> 32) as u32);
        self.push_u32(size as u32);
    }

    pub fn prop_strlist(&mut self, name: &str, vals: &[&str]) {
        let off = self.string_offset(name);
        let total: usize = vals.iter().map(|s| s.len() + 1).sum();

        self.push_u32(FDT_PROP);
        self.push_u32(total as u32);
        self.push_u32(off);

        for s in vals {
            self.structure.extend_from_slice(s.as_bytes());
            self.structure.push(0);
        }

        self.align4();
    }

    pub fn prop_empty(&mut self, name: &str) {
        let off = self.string_offset(name);

        self.push_u32(FDT_PROP);
        self.push_u32(0);
        self.push_u32(off);
    }

    pub fn prop_cells(&mut self, name: &str, cells: &[u32]) {
        let off = self.string_offset(name);

        self.push_u32(FDT_PROP);
        self.push_u32((cells.len() * 4) as u32);
        self.push_u32(off);

        for &c in cells {
            self.push_u32(c);
        }
    }

    pub fn prop_interrupts(&mut self, irq: u32) {
        self.prop_u32("interrupts", irq);
    }

    pub fn prop_interrupt_parent(&mut self, phandle: u32) {
        self.prop_u32("interrupt-parent", phandle);
    }

    pub fn prop_phandle(&mut self, phandle: u32) {
        self.prop_u32("phandle", phandle);
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.push_u32(FDT_END);

        let header_size: u32 = 40;
        let mem_rsvmap_size: u32 = 16;
        let struct_offset = header_size + mem_rsvmap_size;
        let struct_size = self.structure.len() as u32;
        let strings_offset = struct_offset + struct_size;
        let strings_size = self.strings.len() as u32;
        let total_size = strings_offset + strings_size;

        let mut out = Vec::with_capacity(total_size as usize);

        push_be_u32(&mut out, FDT_MAGIC);
        push_be_u32(&mut out, total_size);
        push_be_u32(&mut out, struct_offset);
        push_be_u32(&mut out, strings_offset);
        push_be_u32(&mut out, header_size);
        push_be_u32(&mut out, FDT_VERSION);
        push_be_u32(&mut out, 16);
        push_be_u32(&mut out, 0);
        push_be_u32(&mut out, strings_size);
        push_be_u32(&mut out, struct_size);

        out.extend_from_slice(&[0u8; 16]);

        out.extend_from_slice(&self.structure);
        out.extend_from_slice(&self.strings);
        out
    }

    fn push_u32(&mut self, val: u32) {
        self.structure.extend_from_slice(&val.to_be_bytes());
    }

    fn align4(&mut self) {
        while !self.structure.len().is_multiple_of(4) {
            self.structure.push(0);
        }
    }

    fn string_offset(&mut self, name: &str) -> u32 {
        let needle = name.as_bytes();
        let mut i = 0;
        while i < self.strings.len() {
            let end = i + needle.len();
            if end < self.strings.len() && &self.strings[i..end] == needle && self.strings[end] == 0 {
                return i as u32;
            }

            while i < self.strings.len() && self.strings[i] != 0 {
                i += 1;
            }

            i += 1;
        }

        let off = self.strings.len() as u32;
        self.strings.extend_from_slice(needle);
        self.strings.push(0);

        off
    }
}

fn push_be_u32(buf: &mut Vec<u8>, val: u32) {
    buf.extend_from_slice(&val.to_be_bytes());
}

#[allow(clippy::too_many_arguments)]
pub fn build(
    ram_base: u64,
    ram_size: u64,
    uart_base: u64,
    clint_base: u64,
    plic_base: u64,
    uart_irq: u32,
    timebase_freq: u64,
    virtio_base: u64,
    virtio_size: u64,
    virtio_irqs: &[u32],
    has_blk: bool,
    has_net: bool,
    bootargs: &str,
    initrd_start: u64,
    initrd_end: u64,
) -> Vec<u8> {
    let mut b = DtbBuilder::new();

    // Root node
    b.begin_node("");
    b.prop_u32("#address-cells", 2);
    b.prop_u32("#size-cells", 2);
    b.prop_str("compatible", "riscv-virtio");
    b.prop_str("model", "riscv-virtio,qemu");

    b.begin_node("aliases");
    b.prop_str("serial0", &format!("/uart@{:x}", uart_base));
    b.end_node();

    b.begin_node("chosen");
    b.prop_str("bootargs", bootargs);
    b.prop_str("stdout-path", "serial0:115200n8");
    if initrd_start != 0 {
        b.prop_u64("linux,initrd-start", initrd_start);
        b.prop_u64("linux,initrd-end", initrd_end);
    }
    b.end_node();

    // cpus
    b.begin_node("cpus");
    b.prop_u32("#address-cells", 1);
    b.prop_u32("#size-cells", 0);
    b.prop_u32("timebase-frequency", timebase_freq as u32);

    b.begin_node("cpu@0");
    b.prop_str("device_type", "cpu");
    b.prop_u32("reg", 0);
    b.prop_str("status", "okay");
    b.prop_str("compatible", "riscv");
    b.prop_str("riscv,isa", "rv64imafdcsu_zicsr_zifencei");
    b.prop_strlist("riscv,isa-extensions", &[
        "i", "m", "a", "f", "d", "c", "s", "u", "zicsr", "zifencei",
    ]);
    b.prop_str("mmu-type", "riscv,sv39");
    b.prop_phandle(1);

    b.begin_node("interrupt-controller");
    b.prop_u32("#interrupt-cells", 1);
    b.prop_str("compatible", "riscv,cpu-intc");
    b.prop_empty("interrupt-controller");
    b.prop_phandle(2);
    b.end_node();

    b.end_node(); // cpu@0
    b.end_node(); // cpus

    // memory
    b.begin_node(&format!("memory@{:x}", ram_base));
    b.prop_str("device_type", "memory");
    b.prop_reg(ram_base, ram_size);
    b.end_node();

    // cells
    b.begin_node(&format!("clint@{:x}", clint_base));
    b.prop_str("compatible", "riscv,clint0");
    b.prop_cells("interrupts-extended", &[2, 3, 2, 7]);
    b.prop_reg(clint_base, 0x000c_0000);
    b.end_node();

    // PLIC
    b.begin_node(&format!("plic@{:x}", plic_base));
    b.prop_str("compatible", "sifive,plic-1.0.0");
    b.prop_u32("#interrupt-cells", 1);
    b.prop_u32("#address-cells", 0);
    b.prop_empty("interrupt-controller");
    b.prop_phandle(3);
    b.prop_u32("riscv,ndev", 31);

    b.prop_cells("interrupts-extended", &[2, 9, 2, 11]);
    b.prop_reg(plic_base, 0x0040_0000);
    b.end_node();

    // UART
    b.begin_node(&format!("uart@{:x}", uart_base));
    b.prop_str("compatible", "ns16550a");
    b.prop_reg(uart_base, 0x100);
    b.prop_u32("clock-frequency", 3_686_400);
    b.prop_interrupt_parent(3);
    b.prop_interrupts(uart_irq);
    b.end_node();

    let virtio_names = ["virtio-blk", "virtio-console", "virtio-net"];
    for (i, (&irq, name)) in virtio_irqs.iter().zip(virtio_names.iter()).enumerate() {
        if i == 0 && !has_blk { continue; }
        if i == 2 && !has_net { continue; }
        let base = virtio_base + i as u64 * virtio_size;
        b.begin_node(&format!("virtio_mmio@{:x}", base));
        b.prop_str("compatible", "virtio,mmio");
        b.prop_reg(base, virtio_size);
        b.prop_interrupt_parent(3);
        b.prop_interrupts(irq);
        b.prop_str("device", name);
        b.end_node();
    }

    b.end_node(); // root
    b.finish()
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use crate::{RAM_BASE, UART_BASE, PLIC_BASE, VIRTIO_BASE, VIRTIO_SIZE,
//                     VIRTIO_BLK_IRQ, VIRTIO_CONSOLE_IRQ, VIRTIO_NET_IRQ, UART_IRQ};
//     use crate::clint::{CLINT_BASE, RTC_FREQ};

//     #[test]
//     fn dump_dtb() {
//         let dtb = build(
//             RAM_BASE, 128 * 1024 * 1024,
//             UART_BASE, CLINT_BASE, PLIC_BASE,
//             UART_IRQ, RTC_FREQ,
//             VIRTIO_BASE, VIRTIO_SIZE, &[VIRTIO_BLK_IRQ, VIRTIO_CONSOLE_IRQ, VIRTIO_NET_IRQ],
//             false, false,
//             "root=/dev/ram0 rw console=ttyS0 earlycon",
//             0x84000000, 0x8434b39a,
//         );
//         std::fs::write("/tmp/test.dtb", &dtb).unwrap();
//         println!("DTB size: {} bytes", dtb.len());
//     }
// }
