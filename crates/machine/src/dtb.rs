const FDT_MAGIC: u32 = 0xd00dfeed;
const FDT_BEGIN_NODE: u32 = 0x00000001;
const FDT_END_NODE: u32 = 0x00000002;
const FDT_PROP: u32 = 0x00000003;
const FDT_END: u32 = 0x00000009;
const FDT_VERSION: u32 = 17;

pub struct DtbBuilder {
    structure_block: Vec<u8>,
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
            structure_block: Vec::new(),
            strings: Vec::new(),
        }
    }

    pub fn begin_node(&mut self, name: &str) {
        self.push_u32(FDT_BEGIN_NODE);

        self.structure_block.extend_from_slice(name.as_bytes());
        self.structure_block.push(0);

        self.align_to_4_bytes();
    }

    pub fn end_node(&mut self) {
        self.push_u32(FDT_END_NODE);
    }

    pub fn prop_u32(&mut self, name: &str, value: u32) {
        let offset = self.string_offset(name);

        self.push_u32(FDT_PROP);
        self.push_u32(4);
        self.push_u32(offset);
        self.push_u32(value);
    }

    pub fn prop_u64(&mut self, name: &str, value: u64) {
        let offset = self.string_offset(name);

        self.push_u32(FDT_PROP);
        self.push_u32(8);
        self.push_u32(offset);
        self.push_u32((value >> 32) as u32);
        self.push_u32(value as u32);
    }

    pub fn prop_str(&mut self, name: &str, value: &str) {
        let offset = self.string_offset(name);

        self.push_u32(FDT_PROP);
        self.push_u32((value.len() + 1) as u32);
        self.push_u32(offset);

        self.structure_block.extend_from_slice(value.as_bytes());
        self.structure_block.push(0);

        self.align_to_4_bytes();
    }

    pub fn prop_reg(&mut self, addr: u64, size: u64) {
        let offset = self.string_offset("reg");

        self.push_u32(FDT_PROP);
        self.push_u32(16);
        self.push_u32(offset);
        self.push_u32((addr >> 32) as u32);
        self.push_u32(addr as u32);
        self.push_u32((size >> 32) as u32);
        self.push_u32(size as u32);
    }

    pub fn prop_strlist(&mut self, name: &str, values: &[&str]) {
        let offset = self.string_offset(name);
        let total: usize = values
            .iter()
            .map(|string_value| string_value.len() + 1)
            .sum();

        self.push_u32(FDT_PROP);
        self.push_u32(total as u32);
        self.push_u32(offset);

        for string_value in values {
            self.structure_block
                .extend_from_slice(string_value.as_bytes());
            self.structure_block.push(0);
        }

        self.align_to_4_bytes();
    }

    pub fn prop_empty(&mut self, name: &str) {
        let offset = self.string_offset(name);

        self.push_u32(FDT_PROP);
        self.push_u32(0);
        self.push_u32(offset);
    }

    pub fn prop_cells(&mut self, name: &str, cells: &[u32]) {
        let offset = self.string_offset(name);

        self.push_u32(FDT_PROP);
        self.push_u32((cells.len() * 4) as u32);
        self.push_u32(offset);

        for &cell in cells {
            self.push_u32(cell);
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
        let struct_size = self.structure_block.len() as u32;
        let strings_offset = struct_offset + struct_size;
        let strings_size = self.strings.len() as u32;
        let total_size = strings_offset + strings_size;

        let mut out = Vec::with_capacity(total_size as usize);

        push_big_endian_u32(&mut out, FDT_MAGIC);
        push_big_endian_u32(&mut out, total_size);
        push_big_endian_u32(&mut out, struct_offset);
        push_big_endian_u32(&mut out, strings_offset);
        push_big_endian_u32(&mut out, header_size);
        push_big_endian_u32(&mut out, FDT_VERSION);
        push_big_endian_u32(&mut out, 16);
        push_big_endian_u32(&mut out, 0);
        push_big_endian_u32(&mut out, strings_size);
        push_big_endian_u32(&mut out, struct_size);

        out.extend_from_slice(&[0u8; 16]);

        out.extend_from_slice(&self.structure_block);
        out.extend_from_slice(&self.strings);

        out
    }

    fn push_u32(&mut self, value: u32) {
        self.structure_block.extend_from_slice(&value.to_be_bytes());
    }

    fn align_to_4_bytes(&mut self) {
        while !self.structure_block.len().is_multiple_of(4) {
            self.structure_block.push(0);
        }
    }

    fn string_offset(&mut self, name: &str) -> u32 {
        let needle = name.as_bytes();

        let mut i = 0;
        while i < self.strings.len() {
            let end = i + needle.len();
            if end < self.strings.len() && &self.strings[i..end] == needle && self.strings[end] == 0
            {
                return i as u32;
            }

            while i < self.strings.len() && self.strings[i] != 0 {
                i += 1;
            }

            i += 1;
        }

        let offset = self.strings.len() as u32;
        self.strings.extend_from_slice(needle);
        self.strings.push(0);

        offset
    }
}

fn push_big_endian_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_be_bytes());
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
    let mut builder = DtbBuilder::new();

    // Root node
    builder.begin_node("");
    builder.prop_u32("#address-cells", 2);
    builder.prop_u32("#size-cells", 2);
    builder.prop_str("compatible", "riscv-virtio");
    builder.prop_str("model", "riscv-virtio,qemu");

    builder.begin_node("aliases");
    builder.prop_str("serial0", &format!("/uart@{:x}", uart_base));
    builder.end_node();

    builder.begin_node("chosen");
    builder.prop_str("bootargs", bootargs);
    builder.prop_str("stdout-path", "serial0:115200n8");
    if initrd_start != 0 {
        builder.prop_u64("linux,initrd-start", initrd_start);
        builder.prop_u64("linux,initrd-end", initrd_end);
    }
    builder.end_node();

    // cpus
    builder.begin_node("cpus");
    builder.prop_u32("#address-cells", 1);
    builder.prop_u32("#size-cells", 0);
    builder.prop_u32("timebase-frequency", timebase_freq as u32);

    builder.begin_node("cpu@0");
    builder.prop_str("device_type", "cpu");
    builder.prop_u32("reg", 0);
    builder.prop_str("status", "okay");
    builder.prop_str("compatible", "riscv");
    builder.prop_str("riscv,isa", "rv64imafdcsu_zicsr_zifencei");
    builder.prop_strlist(
        "riscv,isa-extensions",
        &["i", "m", "a", "f", "d", "c", "s", "u", "zicsr", "zifencei"],
    );
    builder.prop_str("mmu-type", "riscv,sv39");
    builder.prop_phandle(1);

    builder.begin_node("interrupt-controller");
    builder.prop_u32("#interrupt-cells", 1);
    builder.prop_str("compatible", "riscv,cpu-intc");
    builder.prop_empty("interrupt-controller");
    builder.prop_phandle(2);
    builder.end_node();

    builder.end_node();
    builder.end_node();

    // memory
    builder.begin_node(&format!("memory@{:x}", ram_base));
    builder.prop_str("device_type", "memory");
    builder.prop_reg(ram_base, ram_size);
    builder.end_node();

    // cells
    builder.begin_node(&format!("clint@{:x}", clint_base));
    builder.prop_str("compatible", "riscv,clint0");
    builder.prop_cells("interrupts-extended", &[2, 3, 2, 7]);
    builder.prop_reg(clint_base, 0x000c_0000);
    builder.end_node();

    // PLIC
    builder.begin_node(&format!("plic@{:x}", plic_base));
    builder.prop_str("compatible", "sifive,plic-1.0.0");
    builder.prop_u32("#interrupt-cells", 1);
    builder.prop_u32("#address-cells", 0);
    builder.prop_empty("interrupt-controller");
    builder.prop_phandle(3);
    builder.prop_u32("riscv,ndev", 31);

    builder.prop_cells("interrupts-extended", &[2, 9, 2, 11]);
    builder.prop_reg(plic_base, 0x0040_0000);
    builder.end_node();

    // UART
    builder.begin_node(&format!("uart@{:x}", uart_base));
    builder.prop_str("compatible", "ns16550a");
    builder.prop_reg(uart_base, 0x100);
    builder.prop_u32("clock-frequency", 3_686_400);
    builder.prop_interrupt_parent(3);
    builder.prop_interrupts(uart_irq);
    builder.end_node();

    let virtio_names = ["virtio-blk", "virtio-console", "virtio-net"];
    for (i, (&irq, name)) in virtio_irqs.iter().zip(virtio_names.iter()).enumerate() {
        if i == 0 && !has_blk {
            continue;
        }

        if i == 2 && !has_net {
            continue;
        }

        let base = virtio_base + i as u64 * virtio_size;
        builder.begin_node(&format!("virtio_mmio@{:x}", base));
        builder.prop_str("compatible", "virtio,mmio");
        builder.prop_reg(base, virtio_size);
        builder.prop_interrupt_parent(3);
        builder.prop_interrupts(irq);
        builder.prop_str("device", name);
        builder.end_node();
    }

    builder.end_node(); // root
    builder.finish()
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
