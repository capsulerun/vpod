// snapshot RAM + physical-address trace -> generated Rust

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Read;

use riscv_core::block::{self, AluKind, Block, Op};
use riscv_core::system_bus::SystemBus;

const RAM_BASE: u64 = 0x8000_0000;

struct SnapshotRam {
    ram: Vec<u8>,
    base: u64,
}

impl SnapshotRam {
    fn idx(&self, address: u64) -> Option<usize> {
        let off = address.checked_sub(self.base)? as usize;
        (off < self.ram.len()).then_some(off)
    }

    fn contains(&self, address: u64, len: u64) -> bool {
        address >= self.base && address + len <= self.base + self.ram.len() as u64
    }
}

impl SystemBus for SnapshotRam {
    fn read_byte(&mut self, address: u64) -> u8 {
        self.idx(address).map_or(0, |i| self.ram[i])
    }

    fn read_halfword(&mut self, address: u64) -> u16 {
        self.idx(address).map_or(0, |i| {
            u16::from_le_bytes(self.ram[i..i + 2].try_into().unwrap())
        })
    }

    fn read_word(&mut self, address: u64) -> u32 {
        self.idx(address).map_or(0, |i| {
            u32::from_le_bytes(self.ram[i..i + 4].try_into().unwrap())
        })
    }

    fn read_doubleword(&mut self, address: u64) -> u64 {
        self.idx(address).map_or(0, |i| {
            u64::from_le_bytes(self.ram[i..i + 8].try_into().unwrap())
        })
    }

    fn write_byte(&mut self, _: u64, _: u8) {}
    fn write_halfword(&mut self, _: u64, _: u16) {}
    fn write_word(&mut self, _: u64, _: u32) {}
    fn write_doubleword(&mut self, _: u64, _: u64) {}
}

fn load_snapshot_ram(path: &str) -> Vec<u8> {
    let raw = std::fs::read(path).unwrap_or_else(|e| {
        eprintln!("cannot read snapshot {path}: {e}");
        std::process::exit(1);
    });

    let mut plain;
    let bytes: &[u8] = if raw.starts_with(b"VPOD") {
        &raw
    } else {
        plain = Vec::new();
        lz4_flex::frame::FrameDecoder::new(&raw[..])
            .read_to_end(&mut plain)
            .unwrap_or_else(|e| {
                eprintln!("snapshot is neither raw VPOD nor lz4: {e}");
                std::process::exit(1);
            });
        &plain
    };

    assert_eq!(&bytes[0..4], b"VPOD", "bad snapshot magic");
    assert_eq!(bytes[4], 1, "unsupported snapshot version");

    let ram_size = u64::from_le_bytes(bytes[6..14].try_into().unwrap()) as usize;
    bytes[14..14 + ram_size].to_vec()
}

fn alu_call(kind: AluKind, lhs: &str, rhs: &str) -> String {
    format!("crate::block::alu(crate::block::AluKind::{kind:?}, {lhs}, {rhs})")
}

struct RegAlloc {
    declared: BTreeSet<u8>,
    written: BTreeSet<u8>,
}

impl RegAlloc {
    fn new() -> Self {
        Self {
            declared: BTreeSet::new(),
            written: BTreeSet::new(),
        }
    }

    fn read(&mut self, out: &mut String, r: u8) -> String {
        if r == 0 {
            return "0u64".to_string();
        }
        if self.declared.insert(r) {
            writeln!(out, "    let mut x{r}: u64 = ctx.regs.read({r});").unwrap();
        }
        format!("x{r}")
    }

    fn write_target(&mut self, out: &mut String, r: u8) -> Option<String> {
        if r == 0 {
            return None;
        }
        if self.declared.insert(r) {
            writeln!(out, "    let mut x{r}: u64;").unwrap();
        }
        self.written.insert(r);
        Some(format!("x{r}"))
    }

    fn writeback(&self) -> String {
        self.written
            .iter()
            .map(|&r| format!("ctx.regs.write({r}, x{r}); "))
            .collect()
    }

    fn reload(&mut self) -> String {
        self.written.clear();
        self.declared
            .iter()
            .map(|&r| format!("x{r} = ctx.regs.read({r}); "))
            .collect()
    }
}

fn emit_block(out: &mut String, pa: u64, entry_seen: &BTreeSet<u64>, blk: &Block) {
    let _ = entry_seen;
    writeln!(
        out,
        "#[allow(unused_variables, unused_mut, unused_assignments)]\nfn block_{pa:x}<B: SystemBus>(ctx: &mut ExecContext<B>, entry_pc: u64, satp: u64) -> (u64, u64) {{"
    )
    .unwrap();

    let mut regs = RegAlloc::new();

    let page_off = pa & 0xfff;
    let next_expr = |insn_off: u64, delta: i64| -> String {
        let target = page_off as i64 + insn_off as i64 + delta;
        if (0..4096).contains(&target) {
            format!(
                "entry_pc.wrapping_add({}u64)",
                (insn_off as i64 + delta) as u64
            )
        } else {
            "u64::MAX".to_string()
        }
    };

    let mut retired: u64 = 0;
    let mut flushed: u64 = 0;

    for insn in blk.ops.iter() {
        let off = insn.pc_off as u64;
        let ilen = insn.ilen as u64;
        let pc = format!("entry_pc.wrapping_add({off})");

        match insn.op {
            Op::Lui { rd, imm } => {
                if let Some(dst) = regs.write_target(out, rd) {
                    writeln!(out, "    {dst} = {imm}i64 as u64;").unwrap();
                }
            }
            Op::Auipc { rd, imm } => {
                if let Some(dst) = regs.write_target(out, rd) {
                    writeln!(out, "    {dst} = {pc}.wrapping_add({imm}i64 as u64);").unwrap();
                }
            }
            Op::AluImm { kind, rd, rs1, imm } => {
                if rd != 0 {
                    let a = regs.read(out, rs1);
                    let expr = alu_call(kind, &a, &format!("{imm}i64 as u64"));
                    let dst = regs.write_target(out, rd).unwrap();
                    writeln!(out, "    {dst} = {expr};").unwrap();
                }
            }
            Op::AluReg { kind, rd, rs1, rs2 } => {
                if rd != 0 {
                    let a = regs.read(out, rs1);
                    let b = regs.read(out, rs2);
                    let expr = alu_call(kind, &a, &b);
                    let dst = regs.write_target(out, rd).unwrap();
                    writeln!(out, "    {dst} = {expr};").unwrap();
                }
            }
            Op::Load { kind, rd, rs1, imm } => {
                let base = regs.read(out, rs1);
                let fault_writeback = regs.writeback();
                let bind = match regs.write_target(out, rd) {
                    None => "Ok(_) => {}".to_string(),
                    Some(dst) => format!("Ok(v) => {dst} = v,"),
                };
                writeln!(
                    out,
                    "    let va = {base}.wrapping_add({imm}i64 as u64);\n    match crate::block::do_load(ctx, satp, crate::block::LoadKind::{kind:?}, va, {pc}) {{\n        {bind}\n        Err(()) => {{ {fault_writeback}ctx.csr.instret = ctx.csr.instret.wrapping_add({}); return ({}, u64::MAX); }}\n    }}",
                    retired - flushed,
                    retired + 1
                )
                .unwrap();
            }
            Op::Store {
                kind,
                rs1,
                rs2,
                imm,
            } => {
                let base = regs.read(out, rs1);
                let val = regs.read(out, rs2);
                let fault_writeback = regs.writeback();
                writeln!(
                    out,
                    "    let va = {base}.wrapping_add({imm}i64 as u64);\n    if crate::block::do_store(ctx, satp, crate::block::StoreKind::{kind:?}, va, {val}, {pc}).is_err() {{\n        {fault_writeback}ctx.csr.instret = ctx.csr.instret.wrapping_add({}); return ({}, u64::MAX);\n    }}",
                    retired - flushed,
                    retired + 1
                )
                .unwrap();
            }
            Op::Branch {
                kind,
                rs1,
                rs2,
                offset,
            } => {
                let cond = {
                    let a = regs.read(out, rs1);
                    let b = regs.read(out, rs2);
                    match kind {
                        block::BranchKind::Beq => format!("{a} == {b}"),
                        block::BranchKind::Bne => format!("{a} != {b}"),
                        block::BranchKind::Blt => format!("({a} as i64) < ({b} as i64)"),
                        block::BranchKind::Bge => format!("({a} as i64) >= ({b} as i64)"),
                        block::BranchKind::Bltu => format!("{a} < {b}"),
                        block::BranchKind::Bgeu => format!("{a} >= {b}"),
                    }
                };
                let writeback = regs.writeback();
                let taken_next = next_expr(off, offset);
                writeln!(
                    out,
                    "    let taken = {cond};\n    if taken {{\n        {writeback}ctx.regs.pc = {pc}.wrapping_add({offset}i64 as u64);\n        ctx.csr.instret = ctx.csr.instret.wrapping_add({});\n        return ({}, {taken_next});\n    }}",
                    retired + 1 - flushed,
                    retired + 1
                )
                .unwrap();
                retired += 1;
            }
            Op::Jal { rd, offset } => {
                let next = next_expr(off, offset);
                if let Some(dst) = regs.write_target(out, rd) {
                    writeln!(out, "    {dst} = {pc}.wrapping_add({ilen});").unwrap();
                }
                writeln!(
                    out,
                    "    {}ctx.regs.pc = {pc}.wrapping_add({offset}i64 as u64);\n    ctx.csr.instret = ctx.csr.instret.wrapping_add({});\n    return ({}, {next});",
                    regs.writeback(),
                    retired + 1 - flushed,
                    retired + 1
                )
                .unwrap();

                retired += 1;
            }
            Op::Jalr { rd, rs1, imm } => {
                let base = regs.read(out, rs1);
                writeln!(
                    out,
                    "    let target = {base}.wrapping_add({imm}i64 as u64) & !1;"
                )
                .unwrap();
                if let Some(dst) = regs.write_target(out, rd) {
                    writeln!(out, "    {dst} = {pc}.wrapping_add({ilen});").unwrap();
                }

                writeln!(
                    out,
                    "    {}ctx.regs.pc = target;\n    ctx.csr.instret = ctx.csr.instret.wrapping_add({});\n    return ({}, if target >> 12 == entry_pc >> 12 {{ target }} else {{ u64::MAX }});",
                    regs.writeback(),
                    retired + 1 - flushed,
                    retired + 1
                )
                .unwrap();

                retired += 1;
            }
            Op::Fallback { raw } => {
                writeln!(
                    out,
                    "    {}ctx.regs.pc = {pc};\n    ctx.csr.instret = ctx.csr.instret.wrapping_add({});\n    let r = crate::execute::exec_raw(ctx, {raw}u32, {pc});\n    debug_assert!(matches!(r, crate::trap::StepResult::Ok));\n    if ctx.regs.pc != {pc}.wrapping_add({ilen}) {{\n        return ({}, u64::MAX);\n    }}\n    {}let satp = crate::block::effective_satp(*ctx.priv_mode, ctx.csr.satp);",
                    regs.writeback(),
                    retired - flushed,
                    retired + 1,
                    regs.reload()
                )
                .unwrap();

                flushed = retired + 1;
                retired += 1;
            }
        }

        if !matches!(
            insn.op,
            Op::Branch { .. } | Op::Jal { .. } | Op::Jalr { .. } | Op::Fallback { .. }
        ) {
            retired += 1;
        }
    }

    let fall_next = next_expr(blk.byte_len as u64, 0);
    writeln!(
        out,
        "    {}ctx.regs.pc = entry_pc.wrapping_add({});\n    ctx.csr.instret = ctx.csr.instret.wrapping_add({});\n    ({retired}, {fall_next})\n}}\n",
        regs.writeback(),
        blk.byte_len,
        retired - flushed
    )
    .unwrap();
}

fn translate_set(bus: &mut SnapshotRam, pas: &BTreeSet<u64>, hot: &BTreeSet<u64>, out_path: &str) {
    let mut out = String::new();
    out.push_str(
        "// Generated by vpod-translate. Do not edit.\nuse crate::execute::ExecContext;\nuse crate::system_bus::SystemBus;\n\n",
    );

    let mut entries: Vec<u64> = Vec::new();
    let mut pages: BTreeSet<u64> = BTreeSet::new();
    let mut skipped = 0usize;

    for &pa in pas {
        if !bus.contains(pa, 4) {
            skipped += 1;
            continue;
        }
        match block::decode_block(bus, pa) {
            Some(blk) => {
                emit_block(&mut out, pa, pas, &blk);
                entries.push(pa);
                pages.insert(pa >> 12);
            }
            None => skipped += 1,
        }
    }

    let mut by_page: std::collections::BTreeMap<u64, (Vec<u64>, Vec<u64>)> =
        std::collections::BTreeMap::new();

    for &pa in &entries {
        let (hot_pas, cold_pas) = by_page.entry(pa >> 12).or_default();
        if hot.contains(&pa) {
            hot_pas.push(pa);
        } else {
            cold_pas.push(pa);
        }
    }

    let mut dispatch = String::new();
    for (page, (_, cold_pas)) in &by_page {
        if cold_pas.is_empty() {
            continue;
        }
        writeln!(
            dispatch,
            "#[inline(never)]\nfn page_{page:x}<B: SystemBus>(ctx: &mut ExecContext<B>, pa: u64, pc: u64, satp: u64) -> Option<(u64, u64)> {{\n    Some(match pa {{"
        )
        .unwrap();
        for pa in cold_pas {
            writeln!(dispatch, "        0x{pa:x} => block_{pa:x}(ctx, pc, satp),").unwrap();
        }
        dispatch.push_str("        _ => return None,\n    })\n}\n\n");
    }

    dispatch.push_str(concat!(
        "#[inline(never)]\npub fn dispatch<B: SystemBus>(ctx: &mut ExecContext<B>, pa_in: u64, entry_pc: u64, satp: u64, fuel: u64, rt_page: u64) -> Option<u64> {\n",
        "    let mut pa = pa_in;\n    let mut pc = entry_pc;\n    let mut total = 0u64;\n",
        "    let mut satp = satp;\n    let mut rt_page = rt_page;\n",
        "    let mut chain_generation = ctx.blocks.aot_evict_generation();\n    loop {\n",
        "        let step = match pa >> 12 {\n",
    ));

    for (page, (hot_pas, cold_pas)) in &by_page {
        let cold_arm = if cold_pas.is_empty() {
            "None".to_string()
        } else {
            format!("page_{page:x}(ctx, pa, pc, satp)")
        };
        if hot_pas.is_empty() {
            writeln!(dispatch, "            0x{page:x} => {cold_arm},").unwrap();
            continue;
        }
        writeln!(dispatch, "            0x{page:x} => match pa {{").unwrap();
        for pa in hot_pas {
            writeln!(
                dispatch,
                "                0x{pa:x} => Some(block_{pa:x}(ctx, pc, satp)),"
            )
            .unwrap();
        }
        writeln!(
            dispatch,
            "                _ => {cold_arm},\n            }},"
        )
        .unwrap();
    }

    dispatch.push_str(concat!(
        "            _ => None,\n        };\n",
        "        let (retired, next) = match step {\n",
        "            Some(v) => v,\n",
        "            None => break,\n",
        "        };\n",
        "        total += retired;\n",
        "        if total >= fuel {\n            return Some(total);\n        }\n",
        "        if next == u64::MAX {\n",
        "            if ctx.csr.pending_interrupt(*ctx.priv_mode).is_some() {\n",
        "                return Some(total);\n",
        "            }\n",
        "            satp = crate::block::effective_satp(*ctx.priv_mode, ctx.csr.satp);\n",
        "            pc = ctx.regs.pc;\n",
        "            let vpage = pc >> 12;\n",
        "            let fetch_pa = if let Some(ppage) = ctx.fetch_tlb.lookup(vpage, satp) {\n",
        "                crate::perf::note_fetch_page_hit();\n",
        "                debug_assert_eq!(\n",
        "                    ctx.mmu.translate_fetch(pc, satp, ctx.bus).map(|pa| pa >> 12),\n",
        "                    Ok(ppage),\n",
        "                    \"fetch TLB hit disagrees with slow-path translation\"\n",
        "                );\n",
        "                (ppage << 12) | (pc & 0xfff)\n",
        "            } else {\n",
        "                crate::perf::note_fetch_translate();\n",
        "                match ctx.mmu.translate_fetch(pc, satp, ctx.bus) {\n",
        "                    Ok(fetch_pa) => {\n",
        "                        ctx.fetch_tlb.insert(vpage, fetch_pa >> 12, satp);\n",
        "                        fetch_pa\n",
        "                    }\n",
        "                    Err(_) => return Some(total),\n",
        "                }\n",
        "            };\n",
        "            match crate::execute::aot_page_key(ctx, fetch_pa) {\n",
        "                Some(key_pa) => {\n",
        "                    pa = key_pa;\n",
        "                    rt_page = fetch_pa >> 12;\n",
        "                    chain_generation = ctx.blocks.aot_evict_generation();\n",
        "                    continue;\n",
        "                }\n",
        "                None => return Some(total),\n",
        "            }\n",
        "        }\n",
        "        pa = (pa & !0xfffu64) | (next & 0xfff);\n",
        "        if ctx.blocks.aot_evict_generation() != chain_generation {\n",
        "            match crate::execute::aot_page_key(ctx, rt_page << 12) {\n",
        "                Some(k) if k >> 12 == pa >> 12 => {}\n",
        "                _ => return Some(total),\n",
        "            }\n",
        "        }\n",
        "        pc = next;\n    }\n",
        "    if total == 0 { None } else { Some(total) }\n}\n\n",
    ));

    let mut pages_str = String::from("pub const AOT_PAGE_HASHES: &[(u64, u64)] = &[\n");
    for &page in &pages {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for i in 0..512u64 {
            hash ^= bus.read_doubleword((page << 12) + i * 8);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        writeln!(pages_str, "    (0x{hash:x}, 0x{page:x}),").unwrap();
    }

    pages_str.push_str("];\n");

    out.push_str(&dispatch);
    out.push_str(&pages_str);

    std::fs::write(out_path, out).unwrap_or_else(|e| {
        eprintln!("cannot write {out_path}: {e}");
        std::process::exit(1);
    });

    eprintln!(
        "[vpod-translate] {} blocks on {} pages ({} pcs skipped) -> {}",
        entries.len(),
        pages.len(),
        skipped,
        out_path
    );
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn r_type(op: u32, f3: u32, f7: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
    op | (rd << 7) | (f3 << 12) | (rs1 << 15) | (rs2 << 20) | (f7 << 25)
}

fn i_type(op: u32, f3: u32, rd: u32, rs1: u32, imm: u32) -> u32 {
    op | (rd << 7) | (f3 << 12) | (rs1 << 15) | ((imm & 0xfff) << 20)
}

fn s_type(op: u32, f3: u32, rs1: u32, rs2: u32, imm: u32) -> u32 {
    op | ((imm & 0x1f) << 7) | (f3 << 12) | (rs1 << 15) | (rs2 << 20) | (((imm >> 5) & 0x7f) << 25)
}

fn b_type(f3: u32, rs1: u32, rs2: u32, imm: u32) -> u32 {
    0x63 | (((imm >> 11) & 1) << 7)
        | (((imm >> 1) & 0xf) << 8)
        | (f3 << 12)
        | (rs1 << 15)
        | (rs2 << 20)
        | (((imm >> 5) & 0x3f) << 25)
        | (((imm >> 12) & 1) << 31)
}

fn rand_rd(rng: &mut Rng) -> u32 {
    let r = 1 + rng.below(15) as u32;
    if r == 10 { 11 } else { r }
}

const PROGRAM_INSNS: usize = 48;
const PAGE: usize = 4096;

fn gen_program(rng: &mut Rng, data_page: u32, code_page: u32) -> Vec<u32> {
    let mut insns: Vec<u32> = Vec::new();

    insns.push(0x37 | (10 << 7) | (data_page << 12));
    insns.push(0x37 | (11 << 7) | (code_page << 12));

    while insns.len() < PROGRAM_INSNS {
        let remaining = PROGRAM_INSNS - insns.len();
        let insn = match rng.below(12) {
            // alu imm
            0..=2 => {
                let f3 = [0u32, 2, 3, 4, 6, 7][rng.below(6) as usize];
                i_type(0x13, f3, rand_rd(rng), rand_rd(rng), rng.next() as u32)
            }
            // shifts
            3 => {
                let (f3, top) = [(1u32, 0u32), (5, 0), (5, 0x400)][rng.below(3) as usize];
                let shamt = (rng.below(64) as u32) | top;
                i_type(0x13, f3, rand_rd(rng), rand_rd(rng), shamt)
            }
            // alu reg
            4..=5 => {
                let (f3, f7) = [
                    (0u32, 0u32),
                    (0, 0x20),
                    (1, 0),
                    (2, 0),
                    (3, 0),
                    (4, 0),
                    (5, 0),
                    (5, 0x20),
                    (6, 0),
                    (7, 0),
                    (0, 1),
                    (4, 1),
                    (5, 1),
                    (6, 1),
                    (7, 1),
                    // Zba: sh1add / sh2add / sh3add
                    (2, 0x10),
                    (4, 0x10),
                    (6, 0x10),
                ][rng.below(18) as usize];
                r_type(0x33, f3, f7, rand_rd(rng), rand_rd(rng), rand_rd(rng))
            }
            // lui / auipc
            6 => {
                let op = if rng.below(2) == 0 { 0x37 } else { 0x17 };
                op | (rand_rd(rng) << 7) | ((rng.next() as u32) & 0xfffff000)
            }
            // load from data page
            7 => {
                let f3 = [0u32, 1, 2, 3, 4, 5, 6][rng.below(7) as usize];
                i_type(0x03, f3, rand_rd(rng), 10, (rng.next() as u32) & 0x7f8)
            }
            // store to data page
            8 => {
                let f3 = [0u32, 1, 2, 3][rng.below(4) as usize];
                s_type(0x23, f3, 10, rand_rd(rng), (rng.next() as u32) & 0x7f8)
            }
            // forward branch
            9 => {
                let f3 = [0u32, 1, 4, 5, 6, 7][rng.below(6) as usize];
                let max_skip = remaining.saturating_sub(1).clamp(1, 4);

                let offset = (4 + 4 * rng.below(max_skip as u64) as u32) & 0x1ffe;
                b_type(f3, rand_rd(rng), rand_rd(rng), offset)
            }
            // Zba
            10 => match rng.below(3) {
                0 => {
                    let f3 = [0u32, 2, 4, 6][rng.below(4) as usize];
                    let f7 = if f3 == 0 { 0x04 } else { 0x10 };
                    r_type(0x3b, f3, f7, rand_rd(rng), rand_rd(rng), rand_rd(rng))
                }
                1 => {
                    // slli.uw carries funct6 = 0x02 above its 6-bit shamt.
                    let shamt = rng.below(64) as u32;
                    i_type(0x1b, 1, rand_rd(rng), rand_rd(rng), (0x02 << 6) | shamt)
                }
                _ => {
                    let rs2 = if rng.below(4) == 0 { rand_rd(rng) } else { 0 };
                    r_type(0x3b, 4, 0x04, rand_rd(rng), rand_rd(rng), rs2)
                }
            },
            // self-modifying store
            _ => {
                let target_insn =
                    (insns.len() as u64 + 1 + rng.below((PROGRAM_INSNS - insns.len()) as u64))
                        .min(PROGRAM_INSNS as u64 - 1);
                s_type(0x23, 2, 11, rand_rd(rng), (target_insn as u32) * 4)
            }
        };
        insns.push(insn);
    }

    insns.push(0x0000_0073);
    insns
}

fn run_gen(args: &[String]) {
    if args.len() != 6 {
        eprintln!(
            "usage: vpod-translate gen <out-dir> <num-programs> <seed> <output-generated.rs>"
        );
        std::process::exit(1);
    }
    let dir = &args[2];
    let num_programs: usize = args[3].parse().expect("bad num-programs");
    let seed: u64 = args[4].parse().expect("bad seed");
    let mut rng = Rng(seed.max(1));

    let ram_len = ((2 * num_programs + 1) * PAGE).next_power_of_two();
    let mut ram = vec![0u8; ram_len];
    let trap_pa = (ram_len - PAGE) as u64;
    ram[trap_pa as usize..trap_pa as usize + 4].copy_from_slice(&0x1050_0073u32.to_le_bytes());

    let mut entry_pcs: Vec<u64> = Vec::new();
    let mut pas: BTreeSet<u64> = BTreeSet::new();

    for i in 0..num_programs {
        let code_base = 2 * i * PAGE;
        let data_page = (2 * i + 1) as u32;
        let insns = gen_program(&mut rng, data_page, (2 * i) as u32);

        for (j, insn) in insns.iter().enumerate() {
            ram[code_base + 4 * j..code_base + 4 * j + 4].copy_from_slice(&insn.to_le_bytes());
            pas.insert((code_base + 4 * j) as u64);
        }
        entry_pcs.push(code_base as u64);
    }

    std::fs::create_dir_all(dir).expect("cannot create out dir");
    std::fs::write(format!("{dir}/ram.bin"), &ram).expect("cannot write ram.bin");
    let entries_txt: String = entry_pcs.iter().map(|pc| format!("{pc:x}\n")).collect();
    std::fs::write(format!("{dir}/entries.txt"), entries_txt).expect("cannot write entries.txt");

    let hot: BTreeSet<u64> = pas.iter().copied().step_by(2).collect();

    let mut bus = SnapshotRam { ram, base: 0 };
    translate_set(&mut bus, &pas, &hot, &args[5]);

    eprintln!(
        "[vpod-translate] gen: {num_programs} programs (seed {seed}), ram {} KiB, trap vector at 0x{trap_pa:x} -> {dir}",
        ram_len / 1024
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() >= 2 && args[1] == "gen" {
        run_gen(&args);
        return;
    }

    if args.len() < 4 {
        eprintln!(
            "usage: vpod-translate <snapshot> <trace-file> <output-generated.rs> [--max-blocks N] [--hot-blocks N] [--coverage PCT]"
        );
        std::process::exit(1);
    }

    let mut max_blocks: usize = 16384;
    let mut hot_blocks: usize = 4096;
    let mut coverage: f64 = 100.0;
    let mut i = 4;
    while i < args.len() {
        match args[i].as_str() {
            "--max-blocks" => {
                max_blocks = args[i + 1].parse().expect("bad --max-blocks");
                i += 2;
            }
            "--hot-blocks" => {
                hot_blocks = args[i + 1].parse().expect("bad --hot-blocks");
                i += 2;
            }
            "--coverage" => {
                coverage = args[i + 1].parse().expect("bad --coverage");
                i += 2;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(1);
            }
        }
    }

    let ram = load_snapshot_ram(&args[1]);
    let trace = std::fs::read_to_string(&args[2]).unwrap_or_else(|e| {
        eprintln!("cannot read trace {}: {e}", args[2]);
        std::process::exit(1);
    });

    let mut bus = SnapshotRam {
        ram,
        base: RAM_BASE,
    };

    let mut counted: Vec<(u64, u64)> = trace
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let pa = u64::from_str_radix(parts.next()?.trim_start_matches("0x"), 16).ok()?;
            let n = parts.next().and_then(|c| c.parse().ok()).unwrap_or(1u64);
            Some((pa, n))
        })
        .collect();

    counted.sort_by_key(|b| std::cmp::Reverse(b.1));

    let total: u64 = counted.iter().map(|&(_, n)| n).sum();
    let target = (total as f64 * coverage / 100.0) as u64;
    let mut cumulative = 0u64;
    let mut pas: BTreeSet<u64> = BTreeSet::new();
    let mut hot: BTreeSet<u64> = BTreeSet::new();

    for &(pa, n) in counted.iter().take(max_blocks) {
        if cumulative >= target {
            break;
        }
        cumulative += n;
        pas.insert(pa);
        if hot.len() < hot_blocks {
            hot.insert(pa);
        }
    }

    eprintln!(
        "[vpod-translate] selected {} of {} traced blocks ({:.2}% of {} block executions)",
        pas.len(),
        counted.len(),
        cumulative as f64 / total.max(1) as f64 * 100.0,
        total
    );

    translate_set(&mut bus, &pas, &hot, &args[3]);
}
