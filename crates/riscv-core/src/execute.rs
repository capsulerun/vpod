// Performs the mathematical and logical calculations requested by the instruction.

use crate::csr::{Csr, PrivMode};
use crate::decode::{Instruction, sign_extend};
use crate::extensions as ext;
use crate::gpr::Gpr;
use crate::mmu::{Mmu, MmuFault};
use crate::system_bus::SystemBus;
use crate::trap::{StepResult, TrapCause};
use crate::vec::exec_vec;

pub const ICACHE_SIZE: usize = 4096;
const ICACHE_TAG_SHIFT: u32 = 1 + ICACHE_SIZE.trailing_zeros();

const OP_LUI: u32 = 0x37;
const OP_AUIPC: u32 = 0x17;
const OP_JAL: u32 = 0x6f;
const OP_JALR: u32 = 0x67;
const OP_BRANCH: u32 = 0x63;
const OP_LOAD: u32 = 0x03;
const OP_STORE: u32 = 0x23;
const OP_IMM: u32 = 0x13;
const OP_IMM32: u32 = 0x1b;
const OP_REG: u32 = 0x33;
const OP_REG32: u32 = 0x3b;
const OP_SYSTEM: u32 = 0x73;
const OP_FENCE: u32 = 0x0f;
const OP_AMO: u32 = 0x2f;
const OP_LOAD_FP: u32 = 0x07;
const OP_STORE_FP: u32 = 0x27;
const OP_OP_FP: u32 = 0x53;
const OP_FMADD: u32 = 0x43;
const OP_FMSUB: u32 = 0x47;
const OP_FNMSUB: u32 = 0x4B;
const OP_FNMADD: u32 = 0x4F;
const OP_VEC: u32 = 0x57;

const FUNCT3_BEQ: u32 = 0x0;
const FUNCT3_BNE: u32 = 0x1;
const FUNCT3_BLT: u32 = 0x4;
const FUNCT3_BGE: u32 = 0x5;
const FUNCT3_BLTU: u32 = 0x6;
const FUNCT3_BGEU: u32 = 0x7;

const FUNCT3_LB: u32 = 0x0;
const FUNCT3_LH: u32 = 0x1;
const FUNCT3_LW: u32 = 0x2;
const FUNCT3_LD: u32 = 0x3;
const FUNCT3_LBU: u32 = 0x4;
const FUNCT3_LHU: u32 = 0x5;
const FUNCT3_LWU: u32 = 0x6;

const FUNCT3_SB: u32 = 0x0;
const FUNCT3_SH: u32 = 0x1;
const FUNCT3_SW: u32 = 0x2;
const FUNCT3_SD: u32 = 0x3;

pub struct ExecContext<'a, B: SystemBus> {
    pub regs: &'a mut Gpr,
    pub csr: &'a mut Csr,
    pub mmu: &'a mut Mmu,
    pub bus: &'a mut B,
    pub priv_mode: &'a mut PrivMode,
    pub lr_addr: &'a mut Option<u64>,
    pub fetch_vpage: &'a mut u64,
    pub fetch_ppage: &'a mut u64,
    pub fetch_satp: &'a mut u64,
    pub vregs: &'a mut Box<[[u8; crate::hart::VLEN_BYTES]; crate::hart::VREG_COUNT]>,

    pub icache_tags: &'a mut Box<[u64; ICACHE_SIZE]>,
    pub icache_data: &'a mut Box<[u32; ICACHE_SIZE]>,
    pub is_waiting: &'a mut bool,
}

fn invalidate_fetch_cache<B: SystemBus>(ctx: &mut ExecContext<B>) {
    *ctx.fetch_vpage = u64::MAX;

    ctx.icache_tags.fill(u64::MAX);
}

pub fn run<B: SystemBus>(ctx: &mut ExecContext<B>, max_steps: u64) -> StepResult {
    for _ in 0..max_steps {
        match step(ctx) {
            StepResult::Ok => {}
            other => return other,
        }
    }

    StepResult::Ok
}

pub fn step<B: SystemBus>(ctx: &mut ExecContext<B>) -> StepResult {
    if let Some(irq) = ctx.csr.pending_interrupt(*ctx.priv_mode) {
        *ctx.fetch_vpage = u64::MAX;
        *ctx.is_waiting = false;
        return take_interrupt(ctx, irq);
    }

    let pc = ctx.regs.pc;
    let effective_satp = if *ctx.priv_mode == PrivMode::M {
        0
    } else {
        ctx.csr.satp
    };

    let virtual_page = pc >> 12;
    let fetch_physical_address =
        if virtual_page == *ctx.fetch_vpage && effective_satp == *ctx.fetch_satp {
            (*ctx.fetch_ppage << 12) | (pc & 0xfff)
        } else {
            let physical_address = match ctx.mmu.translate_fetch(pc, effective_satp, ctx.bus) {
                Ok(pa) => pa,
                Err(fault) => return trap_from_mmu(ctx, fault),
            };
            *ctx.fetch_vpage = virtual_page;
            *ctx.fetch_ppage = physical_address >> 12;
            *ctx.fetch_satp = effective_satp;
            physical_address
        };

    let instruction_encoding = if fetch_physical_address & 0xfff == 0xffe {
        let lo = ctx.bus.read_halfword(fetch_physical_address) as u32;

        if lo & 0x3 != 0x3 {
            lo
        } else {
            let next_pc = pc + 2;
            let next_pa = match ctx.mmu.translate_fetch(next_pc, effective_satp, ctx.bus) {
                Ok(pa) => pa,
                Err(f) => return trap_from_mmu(ctx, f),
            };

            *ctx.fetch_vpage = next_pc >> 12;
            *ctx.fetch_ppage = next_pa >> 12;
            *ctx.fetch_satp = effective_satp;
            let hi = ctx.bus.read_halfword(next_pa) as u32;

            lo | (hi << 16)
        }
    } else {
        let idx = ((fetch_physical_address >> 1) as usize) & (ICACHE_SIZE - 1);
        let tag = fetch_physical_address >> ICACHE_TAG_SHIFT;
        if ctx.icache_tags[idx] == tag {
            ctx.icache_data[idx]
        } else {
            let w = ctx.bus.read_word(fetch_physical_address);
            ctx.icache_tags[idx] = tag;
            ctx.icache_data[idx] = w;
            w
        }
    };

    let result = if instruction_encoding & 0x3 != 0x3 {
        exec_compressed(ctx, instruction_encoding as u16)
    } else {
        let inst = Instruction(instruction_encoding);
        exec_full(ctx, inst, instruction_encoding, pc)
    };

    match result {
        StepResult::Trap(ref cause) => {
            let (mcause_code, tval) = match cause {
                TrapCause::IllegalInstruction(encoding) => (2, *encoding as u64),
                TrapCause::InstructionAddressMisaligned => (0, pc),
                TrapCause::LoadAddressMisaligned => (4, 0),
                TrapCause::StoreAddressMisaligned => (6, 0),
                TrapCause::Breakpoint => (3, pc),
                _ => return result,
            };
            take_exception(ctx, mcause_code, tval);
            StepResult::Ok
        }
        other => other,
    }
}

fn exec_full<B: SystemBus>(
    ctx: &mut ExecContext<B>,
    inst: Instruction,
    raw: u32,
    pc: u64,
) -> StepResult {
    let effective_satp = if *ctx.priv_mode == PrivMode::M {
        0
    } else {
        ctx.csr.satp
    };

    match inst.opcode() {
        OP_LUI => {
            ctx.regs.write(inst.rd(), inst.imm_u() as u64);
            ctx.regs.pc = pc.wrapping_add(4);
        }
        OP_AUIPC => {
            ctx.regs
                .write(inst.rd(), pc.wrapping_add(inst.imm_u() as u64));
            ctx.regs.pc = pc.wrapping_add(4);
        }

        OP_JAL => {
            let target = pc.wrapping_add(inst.imm_j() as u64);
            if target & 0x1 != 0 {
                return StepResult::Trap(TrapCause::InstructionAddressMisaligned);
            }
            ctx.regs.write(inst.rd(), pc.wrapping_add(4));
            ctx.regs.pc = target;
        }

        OP_JALR => {
            let target = ctx.regs.read(inst.rs1()).wrapping_add(inst.imm_i() as u64) & !1;

            if target & 0x1 != 0 {
                return StepResult::Trap(TrapCause::InstructionAddressMisaligned);
            }

            ctx.regs.write(inst.rd(), pc.wrapping_add(4));
            ctx.regs.pc = target;
        }

        OP_BRANCH => {
            let rs1_value = ctx.regs.read(inst.rs1());
            let rs2_value = ctx.regs.read(inst.rs2());

            let branch_taken = match inst.funct3() {
                FUNCT3_BEQ => rs1_value == rs2_value,
                FUNCT3_BNE => rs1_value != rs2_value,
                FUNCT3_BLT => (rs1_value as i64) < (rs2_value as i64),
                FUNCT3_BGE => (rs1_value as i64) >= (rs2_value as i64),
                FUNCT3_BLTU => rs1_value < rs2_value,
                FUNCT3_BGEU => rs1_value >= rs2_value,
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };

            if branch_taken {
                let branch_target = pc.wrapping_add(inst.imm_b() as u64);

                if branch_target & 0x1 != 0 {
                    return StepResult::Trap(TrapCause::InstructionAddressMisaligned);
                }

                ctx.regs.pc = branch_target;
            } else {
                ctx.regs.pc = pc.wrapping_add(4);
            }
        }

        OP_LOAD => {
            let base_address = ctx.regs.read(inst.rs1());
            let virtual_address = base_address.wrapping_add(inst.imm_i() as u64);

            let access_size: u64 = match inst.funct3() {
                FUNCT3_LB | FUNCT3_LBU => 1,
                FUNCT3_LH | FUNCT3_LHU => 2,
                FUNCT3_LW | FUNCT3_LWU => 4,
                FUNCT3_LD => 8,
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };

            let page_offset = virtual_address & 0xFFF;
            let crosses_page_boundary = page_offset + access_size > 0x1000;

            let loaded_value: u64 = if crosses_page_boundary {
                let mut buffer = [0u8; 8];
                for byte_index in 0..access_size {
                    let byte_virtual_address = virtual_address.wrapping_add(byte_index);
                    let byte_physical_address =
                        match ctx
                            .mmu
                            .translate_load(byte_virtual_address, effective_satp, ctx.bus)
                        {
                            Ok(physical_address) => physical_address,
                            Err(fault) => return trap_from_mmu(ctx, fault),
                        };
                    buffer[byte_index as usize] = ctx.bus.read_byte(byte_physical_address);
                }

                match inst.funct3() {
                    FUNCT3_LH => {
                        let halfword = u16::from_le_bytes([buffer[0], buffer[1]]);
                        halfword as i16 as i64 as u64
                    }
                    FUNCT3_LW => {
                        let word = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
                        word as i32 as i64 as u64
                    }
                    FUNCT3_LD => u64::from_le_bytes(buffer),
                    FUNCT3_LHU => u16::from_le_bytes([buffer[0], buffer[1]]) as u64,
                    FUNCT3_LWU => {
                        u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as u64
                    }
                    _ => unreachable!(),
                }
            } else {
                let physical_address =
                    match ctx
                        .mmu
                        .translate_load(virtual_address, effective_satp, ctx.bus)
                    {
                        Ok(physical_address) => physical_address,
                        Err(fault) => return trap_from_mmu(ctx, fault),
                    };

                match inst.funct3() {
                    FUNCT3_LB => {
                        let byte = ctx.bus.read_byte(physical_address);
                        byte as i8 as i64 as u64
                    }
                    FUNCT3_LH => {
                        let halfword = ctx.bus.read_halfword(physical_address);
                        halfword as i16 as i64 as u64
                    }
                    FUNCT3_LW => {
                        let word = ctx.bus.read_word(physical_address);
                        word as i32 as i64 as u64
                    }
                    FUNCT3_LD => ctx.bus.read_doubleword(physical_address),
                    FUNCT3_LBU => ctx.bus.read_byte(physical_address) as u64,
                    FUNCT3_LHU => ctx.bus.read_halfword(physical_address) as u64,
                    FUNCT3_LWU => ctx.bus.read_word(physical_address) as u64,
                    _ => unreachable!(),
                }
            };

            ctx.regs.write(inst.rd(), loaded_value);
            ctx.regs.pc = pc.wrapping_add(4);
        }

        OP_STORE => {
            let base_address = ctx.regs.read(inst.rs1());
            let virtual_address = base_address.wrapping_add(inst.imm_s() as u64);
            let source_value = ctx.regs.read(inst.rs2());

            let store_size: u64 = match inst.funct3() {
                FUNCT3_SB => 1,
                FUNCT3_SH => 2,
                FUNCT3_SW => 4,
                FUNCT3_SD => 8,
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };

            let page_offset = virtual_address & 0xFFF;
            let crosses_page_boundary = page_offset + store_size > 0x1000;

            if crosses_page_boundary {
                let bytes = source_value.to_le_bytes();
                for byte_index in 0..store_size {
                    let byte_virtual_address = virtual_address.wrapping_add(byte_index);
                    let byte_physical_address =
                        match ctx
                            .mmu
                            .translate_store(byte_virtual_address, effective_satp, ctx.bus)
                        {
                            Ok(physical_address) => physical_address,
                            Err(fault) => return trap_from_mmu(ctx, fault),
                        };
                    ctx.bus
                        .write_byte(byte_physical_address, bytes[byte_index as usize]);
                }
            } else {
                let physical_address =
                    match ctx
                        .mmu
                        .translate_store(virtual_address, effective_satp, ctx.bus)
                    {
                        Ok(physical_address) => physical_address,
                        Err(fault) => return trap_from_mmu(ctx, fault),
                    };

                match inst.funct3() {
                    FUNCT3_SB => ctx.bus.write_byte(physical_address, source_value as u8),
                    FUNCT3_SH => ctx
                        .bus
                        .write_halfword(physical_address, source_value as u16),
                    FUNCT3_SW => ctx.bus.write_word(physical_address, source_value as u32),
                    FUNCT3_SD => ctx.bus.write_doubleword(physical_address, source_value),
                    _ => unreachable!(),
                }
            }

            ctx.regs.pc = pc.wrapping_add(4);
        }

        OP_IMM => {
            let rs1 = ctx.regs.read(inst.rs1());
            let imm = inst.imm_i();
            let shamt = (imm & 0x3f) as u32;
            let funct7 = inst.funct7();
            let val = match inst.funct3() {
                0x0 => rs1.wrapping_add(imm as u64),
                0x1 => match funct7 >> 1 {
                    0x00 => rs1 << shamt,
                    // Zbb: clz/ctz/cpop
                    0x18 => match shamt {
                        0 => rs1.leading_zeros() as u64,  // clz
                        1 => rs1.trailing_zeros() as u64, // ctz
                        2 => rs1.count_ones() as u64,     // cpop
                        _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
                    },
                    // Zbb: sext.b/sext.h
                    0x1a => match shamt {
                        2 => rs1 as i8 as i64 as u64,  // sext.b
                        4 => rs1 as i16 as i64 as u64, // sext.h
                        _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
                    },
                    _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
                },
                0x2 => ((rs1 as i64) < imm) as u64,
                0x3 => (rs1 < imm as u64) as u64,
                0x4 => rs1 ^ imm as u64,
                0x5 => match funct7 >> 1 {
                    0x00 => rs1 >> shamt,                    // srli
                    0x10 => ((rs1 as i64) >> shamt) as u64,  // srai
                    0x14 if shamt == 7 => orc_b(rs1),        // Zbb: orc.b
                    0x35 if shamt == 24 => rs1.swap_bytes(), // Zbb: rev8
                    0x35 if shamt == 8 => rs1.swap_bytes(),  // RV32 form
                    0x18 => rs1.rotate_right(shamt),         // Zbb: rori
                    _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
                },
                0x6 => rs1 | imm as u64,
                0x7 => rs1 & imm as u64,
                _ => unreachable!(),
            };
            ctx.regs.write(inst.rd(), val);
            ctx.regs.pc = pc.wrapping_add(4);
        }

        OP_IMM32 => {
            let rs1 = ctx.regs.read(inst.rs1()) as u32;
            let imm = inst.imm_i();
            let shamt = (imm & 0x1f) as u32;
            let funct7 = inst.funct7();

            let val: i32 = match inst.funct3() {
                0x0 => rs1.wrapping_add(imm as u32) as i32,
                0x1 => match funct7 {
                    0x00 => (rs1 << shamt) as i32, // slliw
                    0x30 => match shamt {
                        // Zbb: clzw/ctzw/cpopw
                        0 => rs1.leading_zeros() as i32,  // clzw
                        1 => rs1.trailing_zeros() as i32, // ctzw
                        2 => rs1.count_ones() as i32,     // cpopw
                        _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
                    },
                    _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
                },
                0x5 => match funct7 {
                    0x00 => (rs1 >> shamt) as i32,          // srliw
                    0x20 => (rs1 as i32) >> shamt,          // sraiw
                    0x30 => rs1.rotate_right(shamt) as i32, // roriw (Zbb)
                    _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
                },
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };

            ctx.regs.write(inst.rd(), val as i64 as u64);
            ctx.regs.pc = pc.wrapping_add(4);
        }

        OP_REG => {
            let rs1 = ctx.regs.read(inst.rs1());
            let rs2 = ctx.regs.read(inst.rs2());
            let shamt = (rs2 & 0x3f) as u32;
            let val = match (inst.funct3(), inst.funct7()) {
                // Base ISA
                (0x0, 0x00) => rs1.wrapping_add(rs2),
                (0x0, 0x20) => rs1.wrapping_sub(rs2),
                (0x1, 0x00) => rs1 << shamt,
                (0x2, 0x00) => ((rs1 as i64) < (rs2 as i64)) as u64,
                (0x3, 0x00) => (rs1 < rs2) as u64,
                (0x4, 0x00) => rs1 ^ rs2,
                (0x5, 0x00) => rs1 >> shamt,
                (0x5, 0x20) => ((rs1 as i64) >> shamt) as u64,
                (0x6, 0x00) => rs1 | rs2,
                (0x7, 0x00) => rs1 & rs2,
                // M extension
                (0x0, 0x01) => ext::mul(rs1, rs2),
                (0x1, 0x01) => ext::mulh(rs1, rs2),
                (0x2, 0x01) => ext::mulhsu(rs1, rs2),
                (0x3, 0x01) => ext::mulhu(rs1, rs2),
                (0x4, 0x01) => ext::div(rs1, rs2),
                (0x5, 0x01) => ext::divu(rs1, rs2),
                (0x6, 0x01) => ext::rem(rs1, rs2),
                (0x7, 0x01) => ext::remu(rs1, rs2),
                // Zbb: bitwise with complement
                (0x4, 0x20) => rs1 ^ !rs2, // xnor
                (0x6, 0x20) => rs1 | !rs2, // orn
                (0x7, 0x20) => rs1 & !rs2, // andn
                // Zbb: rotate
                (0x1, 0x30) => rs1.rotate_left(shamt),  // rol
                (0x5, 0x30) => rs1.rotate_right(shamt), // ror
                // Zbb: min/max
                (0x4, 0x05) => {
                    if (rs1 as i64) < (rs2 as i64) {
                        rs1
                    } else {
                        rs2
                    }
                } // min
                (0x5, 0x05) => {
                    if (rs1 as i64) > (rs2 as i64) {
                        rs1
                    } else {
                        rs2
                    }
                } // max
                (0x6, 0x05) => rs1.min(rs2), // minu
                (0x7, 0x05) => rs1.max(rs2), // maxu
                // Zbb: zext.h (pack rs2=x0, funct7=0x04)
                (0x4, 0x04) => rs1 as u16 as u64, // zext.h
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };

            ctx.regs.write(inst.rd(), val);
            ctx.regs.pc = pc.wrapping_add(4);
        }

        OP_REG32 => {
            let rs1 = ctx.regs.read(inst.rs1());
            let rs2 = ctx.regs.read(inst.rs2());

            let val = match (inst.funct3(), inst.funct7()) {
                (0x0, 0x00) => (rs1 as u32).wrapping_add(rs2 as u32) as i32 as i64 as u64,
                (0x0, 0x20) => (rs1 as u32).wrapping_sub(rs2 as u32) as i32 as i64 as u64,
                (0x1, 0x00) => ((rs1 as u32) << (rs2 & 0x1f)) as i32 as i64 as u64,
                (0x5, 0x00) => ((rs1 as u32) >> (rs2 & 0x1f)) as i32 as i64 as u64,
                (0x5, 0x20) => ((rs1 as i32) >> (rs2 & 0x1f)) as i64 as u64,
                (0x0, 0x01) => ext::mulw(rs1, rs2),
                (0x4, 0x01) => ext::divw(rs1, rs2),
                (0x5, 0x01) => ext::divuw(rs1, rs2),
                (0x6, 0x01) => ext::remw(rs1, rs2),
                (0x7, 0x01) => ext::remuw(rs1, rs2),
                // Zbb: rolw/rorw
                (0x1, 0x30) => ((rs1 as u32).rotate_left((rs2 & 0x1f) as u32)) as i32 as i64 as u64,
                (0x5, 0x30) => {
                    ((rs1 as u32).rotate_right((rs2 & 0x1f) as u32)) as i32 as i64 as u64
                }
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };

            ctx.regs.write(inst.rd(), val);
            ctx.regs.pc = pc.wrapping_add(4);
        }

        OP_AMO => return exec_amo(ctx, inst, raw),

        OP_FENCE => {
            ctx.regs.pc = pc.wrapping_add(4);
        }

        OP_SYSTEM => return exec_system(ctx, inst, raw),

        OP_LOAD_FP => return load_fp(ctx, inst, raw, pc),
        OP_STORE_FP => return store_fp(ctx, inst, raw, pc),
        OP_OP_FP => return op_fp(ctx, inst, raw, pc),
        OP_FMADD | OP_FMSUB | OP_FNMSUB | OP_FNMADD => return fma(ctx, inst, raw, pc),
        OP_VEC => return exec_vec(ctx, raw, pc),

        _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
    }

    ctx.csr.instret = ctx.csr.instret.wrapping_add(1);
    StepResult::Ok
}

fn exec_amo<B: SystemBus>(ctx: &mut ExecContext<B>, inst: Instruction, raw: u32) -> StepResult {
    let effective_satp = if *ctx.priv_mode == PrivMode::M {
        0
    } else {
        ctx.csr.satp
    };
    let funct5 = inst.funct7() >> 2;
    let width = inst.funct3();
    let va = ctx.regs.read(inst.rs1());
    let src = ctx.regs.read(inst.rs2());
    let pc = ctx.regs.pc;

    let pa = match ctx.mmu.translate_store(va, effective_satp, ctx.bus) {
        Ok(pa) => pa,
        Err(f) => return trap_from_mmu(ctx, f),
    };

    match funct5 {
        0x02 => {
            let val = if width == 2 {
                ctx.bus.read_word(pa) as i32 as i64 as u64
            } else {
                ctx.bus.read_doubleword(pa)
            };

            ctx.regs.write(inst.rd(), val);
            *ctx.lr_addr = Some(pa);
            ctx.regs.pc = pc.wrapping_add(4);

            return StepResult::Ok;
        }
        0x03 => {
            let success = ctx.lr_addr.is_some_and(|r| r == pa);
            if success {
                if width == 2 {
                    ctx.bus.write_word(pa, src as u32);
                } else {
                    ctx.bus.write_doubleword(pa, src);
                }

                ctx.regs.write(inst.rd(), 0);
            } else {
                ctx.regs.write(inst.rd(), 1);
            }

            *ctx.lr_addr = None;
            ctx.regs.pc = pc.wrapping_add(4);

            return StepResult::Ok;
        }
        _ => {}
    }

    if width == 2 {
        let mem = ctx.bus.read_word(pa);
        let (rd_val, new_val) = match funct5 {
            0x01 => ext::amoswap_w(mem, src),
            0x00 => ext::amoadd_w(mem, src),
            0x04 => ext::amoxor_w(mem, src),
            0x0c => ext::amoand_w(mem, src),
            0x08 => ext::amoor_w(mem, src),
            0x10 => ext::amomin_w(mem, src),
            0x14 => ext::amomax_w(mem, src),
            0x18 => ext::amominu_w(mem, src),
            0x1c => ext::amomaxu_w(mem, src),
            _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
        };

        ctx.bus.write_word(pa, new_val);
        ctx.regs.write(inst.rd(), rd_val);
    } else {
        let mem = ctx.bus.read_doubleword(pa);
        let (rd_val, new_val) = match funct5 {
            0x01 => ext::amoswap_d(mem, src),
            0x00 => ext::amoadd_d(mem, src),
            0x04 => ext::amoxor_d(mem, src),
            0x0c => ext::amoand_d(mem, src),
            0x08 => ext::amoor_d(mem, src),
            0x10 => ext::amomin_d(mem, src),
            0x14 => ext::amomax_d(mem, src),
            0x18 => ext::amominu_d(mem, src),
            0x1c => ext::amomaxu_d(mem, src),
            _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
        };

        ctx.bus.write_doubleword(pa, new_val);
        ctx.regs.write(inst.rd(), rd_val);
    }

    ctx.regs.pc = ctx.regs.pc.wrapping_add(4);
    StepResult::Ok
}

fn exec_system<B: SystemBus>(ctx: &mut ExecContext<B>, inst: Instruction, raw: u32) -> StepResult {
    let pc = ctx.regs.pc;

    match inst.funct3() {
        0x0 => match raw >> 20 {
            0x000 => {
                let cause = match ctx.priv_mode {
                    PrivMode::U => TrapCause::EcallFromUMode,
                    PrivMode::S => TrapCause::EcallFromSMode,
                    PrivMode::M => TrapCause::EcallFromMMode,
                };

                take_exception(ctx, cause.mcause_code(), 0);
                return StepResult::Ok;
            }
            0x001 => {
                // EBREAK but to recheck
                take_exception(ctx, TrapCause::Breakpoint.mcause_code(), pc);
                return StepResult::Ok;
            }
            0x102 => {
                // SRET
                let spp = (ctx.csr.mstatus >> 8) & 1;
                let spie = (ctx.csr.mstatus >> 5) & 1;

                ctx.csr.mstatus &= !crate::csr::MSTATUS_SPP;
                if spie != 0 {
                    ctx.csr.mstatus |= crate::csr::MSTATUS_SIE;
                }

                ctx.csr.mstatus &= !crate::csr::MSTATUS_SPIE;
                *ctx.priv_mode = PrivMode::from_bits(spp);
                ctx.regs.pc = ctx.csr.sepc;
                ctx.mmu.flush();

                invalidate_fetch_cache(ctx);
                return StepResult::Ok;
            }
            0x302 => {
                // MRET
                let mpp = (ctx.csr.mstatus >> 11) & 3;
                let mpie = (ctx.csr.mstatus >> 7) & 1;
                ctx.csr.mstatus &= !crate::csr::MSTATUS_MPP;

                if mpie != 0 {
                    ctx.csr.mstatus |= crate::csr::MSTATUS_MIE;
                }

                ctx.csr.mstatus &= !crate::csr::MSTATUS_MPIE;
                *ctx.priv_mode = PrivMode::from_bits(mpp);
                ctx.regs.pc = ctx.csr.mepc;
                ctx.mmu.flush();

                invalidate_fetch_cache(ctx);
                return StepResult::Ok;
            }
            0x105 => {
                // WFI
                *ctx.is_waiting = true;
                ctx.regs.pc = pc.wrapping_add(4);

                return StepResult::Ok;
            }
            other if (other >> 5) == 0x09 => {
                // SFENCE.VMA
                ctx.mmu.flush();
                invalidate_fetch_cache(ctx);
                ctx.regs.pc = pc.wrapping_add(4);

                return StepResult::Ok;
            }
            _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
        },
        funct3 => {
            let csr_addr = raw >> 20;
            let old = match ctx.csr.read_register(csr_addr, *ctx.priv_mode) {
                Some(v) => v,
                None => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };
            let rs1_val = ctx.regs.read(inst.rs1());
            let uimm = inst.rs1() as u64; // only for CSRRWI/CSRRSI/CSRRCI

            let new_val = match funct3 {
                0x1 => rs1_val,        // CSRRW
                0x2 => old | rs1_val,  // CSRRS
                0x3 => old & !rs1_val, // CSRRC
                0x5 => uimm,           // CSRRWI
                0x6 => old | uimm,     // CSRRSI
                0x7 => old & !uimm,    // CSRRCI
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };

            let do_write = match funct3 {
                0x1 | 0x5 => true,
                _ => inst.rs1() != 0,
            };

            if do_write {
                if !ctx.csr.write_register(csr_addr, new_val, *ctx.priv_mode) {
                    return StepResult::Trap(TrapCause::IllegalInstruction(raw));
                }

                if csr_addr == crate::csr::SATP {
                    ctx.mmu.flush();
                    invalidate_fetch_cache(ctx);
                }
            }

            ctx.regs.write(inst.rd(), old);
            ctx.regs.pc = pc.wrapping_add(4);
        }
    }
    StepResult::Ok
}

enum DwResult {
    Ok(u64),
    Fault(StepResult),
}

#[inline(always)]
fn cross_page_load_dw<B: SystemBus>(va: u64, satp: u64, ctx: &mut ExecContext<B>) -> DwResult {
    if va & 0xFFF <= 0xFF8 {
        match ctx.mmu.translate_load(va, satp, ctx.bus) {
            Ok(pa) => DwResult::Ok(ctx.bus.read_doubleword(pa)),
            Err(f) => DwResult::Fault(trap_from_mmu(ctx, f)),
        }
    } else {
        let mut buf = [0u8; 8];

        for i in 0..8u64 {
            let bva = va.wrapping_add(i);
            match ctx.mmu.translate_load(bva, satp, ctx.bus) {
                Ok(bpa) => buf[i as usize] = ctx.bus.read_byte(bpa),
                Err(f) => return DwResult::Fault(trap_from_mmu(ctx, f)),
            }
        }

        DwResult::Ok(u64::from_le_bytes(buf))
    }
}

#[inline(always)]
fn cross_page_load_dw_result<B: SystemBus>(
    va: u64,
    satp: u64,
    ctx: &mut ExecContext<B>,
) -> Result<u64, MmuFault> {
    if va & 0xFFF <= 0xFF8 {
        let pa = ctx.mmu.translate_load(va, satp, ctx.bus)?;
        Ok(ctx.bus.read_doubleword(pa))
    } else {
        let mut buf = [0u8; 8];

        for i in 0..8u64 {
            let bva = va.wrapping_add(i);
            let bpa = ctx.mmu.translate_load(bva, satp, ctx.bus)?;
            buf[i as usize] = ctx.bus.read_byte(bpa);
        }

        Ok(u64::from_le_bytes(buf))
    }
}

enum DwStoreResult {
    Ok,
    Fault(StepResult),
}

#[inline(always)]
fn cross_page_store_dw<B: SystemBus>(
    va: u64,
    val: u64,
    satp: u64,
    ctx: &mut ExecContext<B>,
) -> DwStoreResult {
    if va & 0xFFF <= 0xFF8 {
        match ctx.mmu.translate_store(va, satp, ctx.bus) {
            Ok(pa) => {
                ctx.bus.write_doubleword(pa, val);
                DwStoreResult::Ok
            }
            Err(f) => DwStoreResult::Fault(trap_from_mmu(ctx, f)),
        }
    } else {
        let bytes = val.to_le_bytes();

        for i in 0..8u64 {
            let bva = va.wrapping_add(i);
            match ctx.mmu.translate_store(bva, satp, ctx.bus) {
                Ok(bpa) => ctx.bus.write_byte(bpa, bytes[i as usize]),
                Err(f) => return DwStoreResult::Fault(trap_from_mmu(ctx, f)),
            }
        }

        DwStoreResult::Ok
    }
}

#[inline(always)]
fn cross_page_store_dw_result<B: SystemBus>(
    va: u64,
    val: u64,
    satp: u64,
    ctx: &mut ExecContext<B>,
) -> Result<(), MmuFault> {
    if va & 0xFFF <= 0xFF8 {
        let pa = ctx.mmu.translate_store(va, satp, ctx.bus)?;
        ctx.bus.write_doubleword(pa, val);
    } else {
        let bytes = val.to_le_bytes();

        for i in 0..8u64 {
            let bva = va.wrapping_add(i);
            let bpa = ctx.mmu.translate_store(bva, satp, ctx.bus)?;
            ctx.bus.write_byte(bpa, bytes[i as usize]);
        }
    }

    Ok(())
}

fn exec_compressed<B: SystemBus>(ctx: &mut ExecContext<B>, raw: u16) -> StepResult {
    let effective_satp = if *ctx.priv_mode == PrivMode::M {
        0
    } else {
        ctx.csr.satp
    };

    let pc = ctx.regs.pc;
    let quad = raw & 0x3;
    let funct = (raw >> 13) as u32;

    macro_rules! rp {
        ($bits:expr) => {
            (($bits & 0x7) as usize) + 8
        };
    }

    match (quad, funct) {
        (0, 0x0) => {
            let nzuimm = (((raw >> 6) & 1) << 2)
                | (((raw >> 5) & 1) << 3)
                | (((raw >> 11) & 0x3) << 4)
                | (((raw >> 7) & 0xf) << 6);

            if nzuimm == 0 {
                return StepResult::Trap(TrapCause::IllegalInstruction(raw as u32));
            }

            let rd = rp!((raw >> 2) as usize);
            ctx.regs
                .write(rd, ctx.regs.read(2).wrapping_add(nzuimm as u64));
            ctx.regs.pc = pc.wrapping_add(2);
        }
        (0, 0x2) => {
            let uimm =
                (((raw >> 6) & 1) << 2) | (((raw >> 10) & 0x7) << 3) | (((raw >> 5) & 1) << 6);
            let rs1 = rp!((raw >> 7) as usize);
            let rd = rp!((raw >> 2) as usize);
            let va = ctx.regs.read(rs1).wrapping_add(uimm as u64);
            let pa = match ctx.mmu.translate_load(va, effective_satp, ctx.bus) {
                Ok(p) => p,
                Err(f) => return trap_from_mmu(ctx, f),
            };

            ctx.regs
                .write(rd, ctx.bus.read_word(pa) as i32 as i64 as u64);
            ctx.regs.pc = pc.wrapping_add(2);
        }
        (0, 0x3) => {
            let uimm = (((raw >> 10) & 0x7) << 3) | (((raw >> 5) & 0x3) << 6);
            let rs1 = rp!((raw >> 7) as usize);
            let rd = rp!((raw >> 2) as usize);
            let va = ctx.regs.read(rs1).wrapping_add(uimm as u64);
            let val = match cross_page_load_dw(va, effective_satp, ctx) {
                DwResult::Ok(v) => v,
                DwResult::Fault(t) => return t,
            };
            ctx.regs.write(rd, val);
            ctx.regs.pc = pc.wrapping_add(2);
        }
        (0, 0x6) => {
            let uimm =
                (((raw >> 6) & 1) << 2) | (((raw >> 10) & 0x7) << 3) | (((raw >> 5) & 1) << 6);
            let rs1 = rp!((raw >> 7) as usize);
            let rs2 = rp!((raw >> 2) as usize);
            let va = ctx.regs.read(rs1).wrapping_add(uimm as u64);
            let pa = match ctx.mmu.translate_store(va, effective_satp, ctx.bus) {
                Ok(p) => p,
                Err(f) => return trap_from_mmu(ctx, f),
            };

            ctx.bus.write_word(pa, ctx.regs.read(rs2) as u32);
            ctx.regs.pc = pc.wrapping_add(2);
        }
        (0, 0x7) => {
            let uimm = (((raw >> 10) & 0x7) << 3) | (((raw >> 5) & 0x3) << 6);
            let rs1 = rp!((raw >> 7) as usize);
            let rs2 = rp!((raw >> 2) as usize);
            let va = ctx.regs.read(rs1).wrapping_add(uimm as u64);

            if let DwStoreResult::Fault(t) =
                cross_page_store_dw(va, ctx.regs.read(rs2), effective_satp, ctx)
            {
                return t;
            }
            ctx.regs.pc = pc.wrapping_add(2);
        }

        (1, 0x0) => {
            // C.ADDI
            let imm = sign_extend((((raw >> 12) & 1) << 5 | ((raw >> 2) & 0x1f)) as i64, 6);
            let rd = ((raw >> 7) & 0x1f) as usize;

            ctx.regs
                .write(rd, ctx.regs.read(rd).wrapping_add(imm as u64));
            ctx.regs.pc = pc.wrapping_add(2);
        }
        (1, 0x1) => {
            // C.ADDIW
            let imm = sign_extend((((raw >> 12) & 1) << 5 | ((raw >> 2) & 0x1f)) as i64, 6);
            let rd = ((raw >> 7) & 0x1f) as usize;
            let val = ctx.regs.read(rd).wrapping_add(imm as u64) as i32 as i64 as u64;

            ctx.regs.write(rd, val);
            ctx.regs.pc = pc.wrapping_add(2);
        }
        (1, 0x2) => {
            // C.LI
            let imm = sign_extend((((raw >> 12) & 1) << 5 | ((raw >> 2) & 0x1f)) as i64, 6);
            let rd = ((raw >> 7) & 0x1f) as usize;

            ctx.regs.write(rd, imm as u64);
            ctx.regs.pc = pc.wrapping_add(2);
        }
        (1, 0x3) => {
            let rd = ((raw >> 7) & 0x1f) as usize;

            if rd == 2 {
                // C.ADDI16SP
                let imm = sign_extend(
                    (((raw >> 12) & 1) << 9
                        | ((raw >> 6) & 1) << 4
                        | ((raw >> 5) & 1) << 6
                        | ((raw >> 3) & 3) << 7
                        | ((raw >> 2) & 1) << 5) as i64,
                    10,
                );

                ctx.regs.write(2, ctx.regs.read(2).wrapping_add(imm as u64));
            } else {
                // C.LUI
                let r32 = raw as u32;
                let imm = sign_extend(
                    ((((r32 >> 12) & 1) << 17) | (((r32 >> 2) & 0x1f) << 12)) as i64,
                    18,
                );

                ctx.regs.write(rd, imm as u64);
            }
            ctx.regs.pc = pc.wrapping_add(2);
        }
        (1, 0x4) => {
            let rd = rp!((raw >> 7) as usize);
            let rs2 = rp!((raw >> 2) as usize);
            let shamt = (((raw >> 12) & 1) << 5 | ((raw >> 2) & 0x1f)) as u32;

            match (raw >> 10) & 0x3 {
                0x0 => ctx.regs.write(rd, ctx.regs.read(rd) >> shamt),
                0x1 => ctx
                    .regs
                    .write(rd, ((ctx.regs.read(rd) as i64) >> shamt) as u64),
                0x2 => {
                    let imm = sign_extend((((raw >> 12) & 1) << 5 | ((raw >> 2) & 0x1f)) as i64, 6);
                    ctx.regs.write(rd, ctx.regs.read(rd) & imm as u64);
                }
                0x3 => match ((raw >> 12) & 1, (raw >> 5) & 0x3) {
                    (0, 0x0) => ctx
                        .regs
                        .write(rd, ctx.regs.read(rd).wrapping_sub(ctx.regs.read(rs2))),
                    (0, 0x1) => ctx.regs.write(rd, ctx.regs.read(rd) ^ ctx.regs.read(rs2)),
                    (0, 0x2) => ctx.regs.write(rd, ctx.regs.read(rd) | ctx.regs.read(rs2)),
                    (0, 0x3) => ctx.regs.write(rd, ctx.regs.read(rd) & ctx.regs.read(rs2)),
                    (1, 0x0) => {
                        let v = (ctx.regs.read(rd) as u32).wrapping_sub(ctx.regs.read(rs2) as u32)
                            as i32 as i64 as u64;
                        ctx.regs.write(rd, v);
                    }
                    (1, 0x1) => {
                        let v = (ctx.regs.read(rd) as u32).wrapping_add(ctx.regs.read(rs2) as u32)
                            as i32 as i64 as u64;
                        ctx.regs.write(rd, v);
                    }
                    _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw as u32)),
                },
                _ => unreachable!(),
            }
            ctx.regs.pc = pc.wrapping_add(2);
        }
        (1, 0x5) => {
            // C.J
            let imm = c_j_imm(raw);
            let target = pc.wrapping_add(imm as u64);
            ctx.regs.pc = target;
        }
        (1, 0x6) => {
            // C.BEQZ
            let rs1 = rp!((raw >> 7) as usize);
            let imm = c_b_imm(raw);

            if ctx.regs.read(rs1) == 0 {
                ctx.regs.pc = pc.wrapping_add(imm as u64);
            } else {
                ctx.regs.pc = pc.wrapping_add(2);
            }
        }
        (1, 0x7) => {
            // C.BNEZ
            let rs1 = rp!((raw >> 7) as usize);
            let imm = c_b_imm(raw);

            if ctx.regs.read(rs1) != 0 {
                ctx.regs.pc = pc.wrapping_add(imm as u64);
            } else {
                ctx.regs.pc = pc.wrapping_add(2);
            }
        }
        (2, 0x0) => {
            // C.SLLI
            let shamt = (((raw >> 12) & 1) << 5 | ((raw >> 2) & 0x1f)) as u32;
            let rd = ((raw >> 7) & 0x1f) as usize;

            ctx.regs.write(rd, ctx.regs.read(rd) << shamt);
            ctx.regs.pc = pc.wrapping_add(2);
        }
        (2, 0x2) => {
            // C.LWSP
            let uimm =
                (((raw >> 12) & 1) << 5) | (((raw >> 4) & 0x7) << 2) | (((raw >> 2) & 0x3) << 6);
            let rd = ((raw >> 7) & 0x1f) as usize;
            let va = ctx.regs.read(2).wrapping_add(uimm as u64);
            let pa = match ctx.mmu.translate_load(va, effective_satp, ctx.bus) {
                Ok(p) => p,
                Err(f) => return trap_from_mmu(ctx, f),
            };

            ctx.regs
                .write(rd, ctx.bus.read_word(pa) as i32 as i64 as u64);
            ctx.regs.pc = pc.wrapping_add(2);
        }
        (2, 0x3) => {
            // C.LDSP
            let uimm =
                (((raw >> 12) & 1) << 5) | (((raw >> 5) & 0x3) << 3) | (((raw >> 2) & 0x7) << 6);
            let rd = ((raw >> 7) & 0x1f) as usize;
            let va = ctx.regs.read(2).wrapping_add(uimm as u64);
            let val = match cross_page_load_dw(va, effective_satp, ctx) {
                DwResult::Ok(v) => v,
                DwResult::Fault(t) => return t,
            };

            ctx.regs.write(rd, val);
            ctx.regs.pc = pc.wrapping_add(2);
        }
        (2, 0x4) => {
            let rd = ((raw >> 7) & 0x1f) as usize;
            let rs2 = ((raw >> 2) & 0x1f) as usize;

            if (raw >> 12) & 1 == 0 {
                if rs2 == 0 {
                    // C.JR
                    ctx.regs.pc = ctx.regs.read(rd) & !1;
                } else {
                    // C.MV
                    ctx.regs.write(rd, ctx.regs.read(rs2));
                    ctx.regs.pc = pc.wrapping_add(2);
                }
            } else if rd == 0 && rs2 == 0 {
                // C.EBREAK
                ctx.regs.pc = pc.wrapping_add(2);
                return StepResult::Trap(TrapCause::Breakpoint);
            } else if rs2 == 0 {
                // C.JALR
                let target = ctx.regs.read(rd) & !1;
                ctx.regs.write(1, pc.wrapping_add(2));
                ctx.regs.pc = target;
            } else {
                // C.ADD
                ctx.regs
                    .write(rd, ctx.regs.read(rd).wrapping_add(ctx.regs.read(rs2)));
                ctx.regs.pc = pc.wrapping_add(2);
            }
        }
        (2, 0x6) => {
            // C.SWSP
            let uimm = (((raw >> 9) & 0xf) << 2) | (((raw >> 7) & 0x3) << 6);
            let rs2 = ((raw >> 2) & 0x1f) as usize;
            let va = ctx.regs.read(2).wrapping_add(uimm as u64);
            let pa = match ctx.mmu.translate_store(va, effective_satp, ctx.bus) {
                Ok(p) => p,
                Err(f) => return trap_from_mmu(ctx, f),
            };

            ctx.bus.write_word(pa, ctx.regs.read(rs2) as u32);
            ctx.regs.pc = pc.wrapping_add(2);
        }
        (2, 0x7) => {
            // C.SDSP
            let uimm = (((raw >> 10) & 0x7) << 3) | (((raw >> 7) & 0x7) << 6);
            let rs2 = ((raw >> 2) & 0x1f) as usize;
            let va = ctx.regs.read(2).wrapping_add(uimm as u64);

            if let DwStoreResult::Fault(t) =
                cross_page_store_dw(va, ctx.regs.read(rs2), effective_satp, ctx)
            {
                return t;
            }
            ctx.regs.pc = pc.wrapping_add(2);
        }

        // C.FLD
        (0, 0x1) => {
            let uimm = (((raw >> 10) & 0x7) << 3) | (((raw >> 5) & 0x3) << 6);
            let rs1 = rp!((raw >> 7) as usize);
            let rd = rp!((raw >> 2) as usize);
            let va = ctx.regs.read(rs1).wrapping_add(uimm as u64);
            let val = match cross_page_load_dw(va, effective_satp, ctx) {
                DwResult::Ok(v) => v,
                DwResult::Fault(t) => return t,
            };

            ctx.regs.write_f(rd, val);
            ctx.regs.pc = pc.wrapping_add(2);
        }
        // C.FSD
        (0, 0x5) => {
            let uimm = (((raw >> 10) & 0x7) << 3) | (((raw >> 5) & 0x3) << 6);
            let rs1 = rp!((raw >> 7) as usize);
            let rs2 = rp!((raw >> 2) as usize);
            let va = ctx.regs.read(rs1).wrapping_add(uimm as u64);

            if let DwStoreResult::Fault(t) =
                cross_page_store_dw(va, ctx.regs.read_f(rs2), effective_satp, ctx)
            {
                return t;
            }

            ctx.regs.pc = pc.wrapping_add(2);
        }
        // C.FLDSP
        (2, 0x1) => {
            let uimm =
                (((raw >> 12) & 1) << 5) | (((raw >> 5) & 0x3) << 3) | (((raw >> 2) & 0x7) << 6);
            let rd = ((raw >> 7) & 0x1f) as usize;
            let va = ctx.regs.read(2).wrapping_add(uimm as u64);
            let val = match cross_page_load_dw(va, effective_satp, ctx) {
                DwResult::Ok(v) => v,
                DwResult::Fault(t) => return t,
            };

            ctx.regs.write_f(rd, val);
            ctx.regs.pc = pc.wrapping_add(2);
        }
        // C.FSDSP
        (2, 0x5) => {
            let uimm = (((raw >> 10) & 0x7) << 3) | (((raw >> 7) & 0x7) << 6);
            let rs2 = ((raw >> 2) & 0x1f) as usize;
            let va = ctx.regs.read(2).wrapping_add(uimm as u64);
            if let DwStoreResult::Fault(t) =
                cross_page_store_dw(va, ctx.regs.read_f(rs2), effective_satp, ctx)
            {
                return t;
            }
            ctx.regs.pc = pc.wrapping_add(2);
        }

        _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw as u32)),
    }

    ctx.csr.instret = ctx.csr.instret.wrapping_add(1);
    StepResult::Ok
}

fn take_interrupt<B: SystemBus>(ctx: &mut ExecContext<B>, irq_bit: u64) -> StepResult {
    let cause = (1u64 << 63) | irq_bit;
    let deleg = ctx.csr.mideleg & (1 << irq_bit);
    if deleg != 0 && *ctx.priv_mode != PrivMode::M {
        ctx.csr.scause = cause;
        ctx.csr.sepc = ctx.regs.pc;
        ctx.csr.stval = 0;

        let spie = (ctx.csr.mstatus & crate::csr::MSTATUS_SIE) >> 1;
        ctx.csr.mstatus &= !crate::csr::MSTATUS_SPIE;
        ctx.csr.mstatus |= spie << 5;
        ctx.csr.mstatus &= !crate::csr::MSTATUS_SIE;

        let spp = *ctx.priv_mode as u64;
        ctx.csr.mstatus = (ctx.csr.mstatus & !crate::csr::MSTATUS_SPP) | (spp << 8);
        *ctx.priv_mode = PrivMode::S;
        ctx.regs.pc = ctx.csr.stvec & !3;
    } else {
        ctx.csr.mcause = cause;
        ctx.csr.mepc = ctx.regs.pc;
        ctx.csr.mtval = 0;

        let mpie = (ctx.csr.mstatus & crate::csr::MSTATUS_MIE) >> 3;
        ctx.csr.mstatus &= !crate::csr::MSTATUS_MPIE;
        ctx.csr.mstatus |= mpie << 7;
        ctx.csr.mstatus &= !crate::csr::MSTATUS_MIE;

        let mpp = *ctx.priv_mode as u64;
        ctx.csr.mstatus = (ctx.csr.mstatus & !crate::csr::MSTATUS_MPP) | (mpp << 11);
        *ctx.priv_mode = PrivMode::M;
        ctx.regs.pc = ctx.csr.mtvec & !3;
    }

    ctx.mmu.flush();
    invalidate_fetch_cache(ctx);
    StepResult::Ok
}

pub fn take_exception<B: SystemBus>(ctx: &mut ExecContext<B>, cause: u64, tval: u64) {
    let deleg = ctx.csr.medeleg & (1 << (cause & 63));

    if deleg != 0 && *ctx.priv_mode != PrivMode::M {
        ctx.csr.scause = cause;
        ctx.csr.sepc = ctx.regs.pc;
        ctx.csr.stval = tval;

        let spie = (ctx.csr.mstatus & crate::csr::MSTATUS_SIE) >> 1;
        ctx.csr.mstatus &= !crate::csr::MSTATUS_SPIE;
        ctx.csr.mstatus |= spie << 5;
        ctx.csr.mstatus &= !crate::csr::MSTATUS_SIE;

        let spp = *ctx.priv_mode as u64;
        ctx.csr.mstatus = (ctx.csr.mstatus & !crate::csr::MSTATUS_SPP) | (spp << 8);
        *ctx.priv_mode = PrivMode::S;
        ctx.regs.pc = ctx.csr.stvec & !3;
    } else {
        ctx.csr.mcause = cause;
        ctx.csr.mepc = ctx.regs.pc;
        ctx.csr.mtval = tval;

        let mpie = (ctx.csr.mstatus & crate::csr::MSTATUS_MIE) >> 3;
        ctx.csr.mstatus &= !crate::csr::MSTATUS_MPIE;
        ctx.csr.mstatus |= mpie << 7;
        ctx.csr.mstatus &= !crate::csr::MSTATUS_MIE;

        let mpp = *ctx.priv_mode as u64;
        ctx.csr.mstatus = (ctx.csr.mstatus & !crate::csr::MSTATUS_MPP) | (mpp << 11);
        *ctx.priv_mode = PrivMode::M;
        ctx.regs.pc = ctx.csr.mtvec & !3;
    }

    ctx.mmu.flush();
    invalidate_fetch_cache(ctx);
}

// Zbb: orc.b
#[inline(always)]
fn orc_b(x: u64) -> u64 {
    let has_zero = x.wrapping_sub(0x0101010101010101) & !x & 0x8080808080808080;

    let _zero_bytes = has_zero
        | (has_zero << 1)
        | (has_zero << 2)
        | (has_zero << 3)
        | (has_zero << 4)
        | (has_zero << 5)
        | (has_zero << 6)
        | (has_zero << 7);
    let mut result = 0u64;

    for i in 0..8 {
        let byte = (x >> (i * 8)) & 0xFF;
        let out = if byte != 0 { 0xFF } else { 0x00 };
        result |= out << (i * 8);
    }

    result
}

fn trap_from_mmu<B: SystemBus>(ctx: &mut ExecContext<B>, f: MmuFault) -> StepResult {
    take_exception(ctx, f.mcause(), f.tval());
    StepResult::Ok
}

fn c_j_imm(raw: u16) -> i64 {
    let r = raw as u32;
    let val = (((r >> 12) & 1) << 11)
        | (((r >> 11) & 1) << 4)
        | (((r >> 9) & 3) << 8)
        | (((r >> 8) & 1) << 10)
        | (((r >> 7) & 1) << 6)
        | (((r >> 6) & 1) << 7)
        | (((r >> 3) & 7) << 1)
        | (((r >> 2) & 1) << 5);

    sign_extend(val as i64, 12)
}

fn c_b_imm(raw: u16) -> i64 {
    let r = raw as u32;
    let val = (((r >> 12) & 1) << 8)
        | (((r >> 10) & 3) << 3)
        | (((r >> 5) & 3) << 6)
        | (((r >> 3) & 3) << 1)
        | (((r >> 2) & 1) << 5);

    sign_extend(val as i64, 9)
}

fn check_fs<B: SystemBus>(ctx: &ExecContext<B>, raw: u32) -> Option<StepResult> {
    let fs = (ctx.csr.mstatus >> 13) & 3;

    if fs == 0 {
        Some(StepResult::Trap(TrapCause::IllegalInstruction(raw)))
    } else {
        None
    }
}

fn mark_fs_dirty<B: SystemBus>(ctx: &mut ExecContext<B>) {
    ctx.csr.mstatus |= crate::csr::MSTATUS_FS;
}

fn load_fp<B: SystemBus>(
    ctx: &mut ExecContext<B>,
    inst: Instruction,
    raw: u32,
    pc: u64,
) -> StepResult {
    if matches!(inst.funct3(), 0 | 5 | 6 | 7) {
        return exec_vec(ctx, raw, pc);
    }

    if let Some(t) = check_fs(ctx, raw) {
        return t;
    }

    let va = ctx.regs.read(inst.rs1()).wrapping_add(inst.imm_i() as u64);
    let satp = if *ctx.priv_mode == PrivMode::M {
        0
    } else {
        ctx.csr.satp
    };

    match inst.funct3() {
        0x2 => {
            // FLW
            let pa = match ctx.mmu.translate_load(va, satp, ctx.bus) {
                Ok(pa) => pa,
                Err(f) => {
                    take_exception(ctx, f.mcause(), f.tval());
                    return StepResult::Ok;
                }
            };
            let val = ctx.bus.read_word(pa);
            ctx.regs.write_f32(inst.rd(), val);
        }
        0x3 => {
            // FLD
            let val = match cross_page_load_dw_result(va, satp, ctx) {
                Ok(v) => v,
                Err(f) => {
                    take_exception(ctx, f.mcause(), f.tval());
                    return StepResult::Ok;
                }
            };
            ctx.regs.write_f(inst.rd(), val);
        }
        _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
    }

    mark_fs_dirty(ctx);
    ctx.regs.pc = pc.wrapping_add(4);
    ctx.csr.instret = ctx.csr.instret.wrapping_add(1);

    StepResult::Ok
}

fn store_fp<B: SystemBus>(
    ctx: &mut ExecContext<B>,
    inst: Instruction,
    raw: u32,
    pc: u64,
) -> StepResult {
    if matches!(inst.funct3(), 0 | 5 | 6 | 7) {
        return exec_vec(ctx, raw, pc);
    }

    if let Some(t) = check_fs(ctx, raw) {
        return t;
    }

    let va = ctx.regs.read(inst.rs1()).wrapping_add(inst.imm_s() as u64);
    let satp = if *ctx.priv_mode == PrivMode::M {
        0
    } else {
        ctx.csr.satp
    };

    match inst.funct3() {
        0x2 => {
            // FSW
            let pa = match ctx.mmu.translate_store(va, satp, ctx.bus) {
                Ok(pa) => pa,
                Err(f) => {
                    take_exception(ctx, f.mcause(), f.tval());
                    return StepResult::Ok;
                }
            };
            let val = ctx.regs.read_f32(inst.rs2());
            ctx.bus.write_word(pa, val);
        }
        0x3 => {
            // FSD
            let val = ctx.regs.read_f(inst.rs2());
            match cross_page_store_dw_result(va, val, satp, ctx) {
                Ok(()) => {}
                Err(f) => {
                    take_exception(ctx, f.mcause(), f.tval());
                    return StepResult::Ok;
                }
            }
        }
        _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
    }

    ctx.regs.pc = pc.wrapping_add(4);
    ctx.csr.instret = ctx.csr.instret.wrapping_add(1);
    StepResult::Ok
}

fn op_fp<B: SystemBus>(
    ctx: &mut ExecContext<B>,
    inst: Instruction,
    raw: u32,
    pc: u64,
) -> StepResult {
    if let Some(t) = check_fs(ctx, raw) {
        return t;
    }

    let funct7 = inst.funct7();
    let rs1 = inst.rs1();
    let rs2 = inst.rs2();
    let rd = inst.rd();

    match funct7 {
        // FMV.W.X
        0x78 => {
            let val = ctx.regs.read(rs1) as u32;
            ctx.regs.write_f32(rd, val);
            mark_fs_dirty(ctx);
        }
        // FMV.X.W
        0x70 => {
            let val = ctx.regs.read_f32(rs1);
            ctx.regs.write(rd, val as i32 as i64 as u64);
        }
        // FMV.D.X
        0x79 => {
            let val = ctx.regs.read(rs1);
            ctx.regs.write_f(rd, val);
            mark_fs_dirty(ctx);
        }
        // FMV.X.D
        0x71 => {
            let val = ctx.regs.read_f(rs1);
            ctx.regs.write(rd, val);
        }
        0x00 => {
            // FADD.S
            let a = f32::from_bits(ctx.regs.read_f32(rs1));
            let b = f32::from_bits(ctx.regs.read_f32(rs2));
            ctx.regs.write_f32(rd, (a + b).to_bits());
            mark_fs_dirty(ctx);
        }
        0x04 => {
            // FSUB.S
            let a = f32::from_bits(ctx.regs.read_f32(rs1));
            let b = f32::from_bits(ctx.regs.read_f32(rs2));
            ctx.regs.write_f32(rd, (a - b).to_bits());
            mark_fs_dirty(ctx);
        }
        0x08 => {
            // FMUL.S
            let a = f32::from_bits(ctx.regs.read_f32(rs1));
            let b = f32::from_bits(ctx.regs.read_f32(rs2));
            ctx.regs.write_f32(rd, (a * b).to_bits());
            mark_fs_dirty(ctx);
        }
        0x0C => {
            // FDIV.S
            let a = f32::from_bits(ctx.regs.read_f32(rs1));
            let b = f32::from_bits(ctx.regs.read_f32(rs2));
            ctx.regs.write_f32(rd, (a / b).to_bits());
            mark_fs_dirty(ctx);
        }
        0x01 => {
            // FADD.D
            let a = f64::from_bits(ctx.regs.read_f(rs1));
            let b = f64::from_bits(ctx.regs.read_f(rs2));
            ctx.regs.write_f(rd, (a + b).to_bits());
            mark_fs_dirty(ctx);
        }
        0x05 => {
            // FSUB.D
            let a = f64::from_bits(ctx.regs.read_f(rs1));
            let b = f64::from_bits(ctx.regs.read_f(rs2));
            ctx.regs.write_f(rd, (a - b).to_bits());
            mark_fs_dirty(ctx);
        }
        0x09 => {
            // FMUL.D
            let a = f64::from_bits(ctx.regs.read_f(rs1));
            let b = f64::from_bits(ctx.regs.read_f(rs2));
            ctx.regs.write_f(rd, (a * b).to_bits());
            mark_fs_dirty(ctx);
        }
        0x0D => {
            // FDIV.D
            let a = f64::from_bits(ctx.regs.read_f(rs1));
            let b = f64::from_bits(ctx.regs.read_f(rs2));
            ctx.regs.write_f(rd, (a / b).to_bits());
            mark_fs_dirty(ctx);
        }
        0x2C => {
            // FSQRT.S
            let a = f32::from_bits(ctx.regs.read_f32(rs1));
            ctx.regs.write_f32(rd, a.sqrt().to_bits());
            mark_fs_dirty(ctx);
        }
        0x2D => {
            // FSQRT.D
            let a = f64::from_bits(ctx.regs.read_f(rs1));

            ctx.regs.write_f(rd, a.sqrt().to_bits());
            mark_fs_dirty(ctx);
        }
        0x10 => {
            // FSGNJ/FSGNJN/FSGNJX.S
            let a = ctx.regs.read_f32(rs1);
            let b = ctx.regs.read_f32(rs2);
            let result = match inst.funct3() {
                0x0 => (a & 0x7FFF_FFFF) | (b & 0x8000_0000), // FSGNJ
                0x1 => (a & 0x7FFF_FFFF) | ((b ^ 0x8000_0000) & 0x8000_0000), // FSGNJN
                0x2 => a ^ (b & 0x8000_0000),                 // FSGNJX
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };

            ctx.regs.write_f32(rd, result);
            mark_fs_dirty(ctx);
        }
        0x11 => {
            // FSGNJ/FSGNJN/FSGNJX.D
            let a = ctx.regs.read_f(rs1);
            let b = ctx.regs.read_f(rs2);
            let sign_mask = 1u64 << 63;
            let result = match inst.funct3() {
                0x0 => (a & !sign_mask) | (b & sign_mask),
                0x1 => (a & !sign_mask) | ((b ^ sign_mask) & sign_mask),
                0x2 => a ^ (b & sign_mask),
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };

            ctx.regs.write_f(rd, result);
            mark_fs_dirty(ctx);
        }
        0x14 => {
            // FMIN/FMAX.S
            let a = f32::from_bits(ctx.regs.read_f32(rs1));
            let b = f32::from_bits(ctx.regs.read_f32(rs2));
            let result = match inst.funct3() {
                0x0 => a.min(b),
                0x1 => a.max(b),
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };

            ctx.regs.write_f32(rd, result.to_bits());
            mark_fs_dirty(ctx);
        }
        0x15 => {
            // FMIN/FMAX.D
            let a = f64::from_bits(ctx.regs.read_f(rs1));
            let b = f64::from_bits(ctx.regs.read_f(rs2));
            let result = match inst.funct3() {
                0x0 => a.min(b),
                0x1 => a.max(b),
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };

            ctx.regs.write_f(rd, result.to_bits());
            mark_fs_dirty(ctx);
        }
        0x50 => {
            // FEQ/FLT/FLE.S
            let a = f32::from_bits(ctx.regs.read_f32(rs1));
            let b = f32::from_bits(ctx.regs.read_f32(rs2));
            let result = match inst.funct3() {
                0x2 => a == b,
                0x1 => a < b,
                0x0 => a <= b,
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };

            ctx.regs.write(rd, result as u64);
        }
        0x51 => {
            // FEQ/FLT/FLE.D
            let a = f64::from_bits(ctx.regs.read_f(rs1));
            let b = f64::from_bits(ctx.regs.read_f(rs2));
            let result = match inst.funct3() {
                0x2 => a == b,
                0x1 => a < b,
                0x0 => a <= b,
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };
            ctx.regs.write(rd, result as u64);
        }
        0x60 => {
            // FCVT.W.S / FCVT.WU.S / FCVT.L.S / FCVT.LU.S
            let a = f32::from_bits(ctx.regs.read_f32(rs1));
            let result = match rs2 {
                0 => (a as i32) as i64 as u64, // FCVT.W.S
                1 => (a as u32) as u64,        // FCVT.WU.S
                2 => (a as i64) as u64,        // FCVT.L.S
                3 => a as u64,                 // FCVT.LU.S
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };

            ctx.regs.write(rd, result);
        }
        0x68 => {
            // FCVT.S.W / FCVT.S.WU / FCVT.S.L / FCVT.S.LU
            let val = ctx.regs.read(rs1);
            let result = match rs2 {
                0 => (val as i32 as f32).to_bits(),
                1 => (val as u32 as f32).to_bits(),
                2 => (val as i64 as f32).to_bits(),
                3 => (val as f32).to_bits(),
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };

            ctx.regs.write_f32(rd, result);
            mark_fs_dirty(ctx);
        }
        0x61 => {
            // FCVT.W.D / FCVT.WU.D / FCVT.L.D / FCVT.LU.D
            let a = f64::from_bits(ctx.regs.read_f(rs1));
            let result = match rs2 {
                0 => (a as i32) as i64 as u64,
                1 => (a as u32) as u64,
                2 => (a as i64) as u64,
                3 => a as u64,
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };

            ctx.regs.write(rd, result);
        }
        0x69 => {
            // FCVT.D.W / FCVT.D.WU / FCVT.D.L / FCVT.D.LU
            let val = ctx.regs.read(rs1);
            let result = match rs2 {
                0 => (val as i32 as f64).to_bits(),
                1 => (val as u32 as f64).to_bits(),
                2 => (val as i64 as f64).to_bits(),
                3 => (val as f64).to_bits(),
                _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
            };

            ctx.regs.write_f(rd, result);
            mark_fs_dirty(ctx);
        }
        0x20 => {
            // FCVT.S.D
            let a = f64::from_bits(ctx.regs.read_f(rs1));
            ctx.regs.write_f32(rd, (a as f32).to_bits());
            mark_fs_dirty(ctx);
        }
        0x21 => {
            // FCVT.D.S
            let a = f32::from_bits(ctx.regs.read_f32(rs1));
            ctx.regs.write_f(rd, (a as f64).to_bits());
            mark_fs_dirty(ctx);
        }
        _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
    }
    ctx.regs.pc = pc.wrapping_add(4);
    ctx.csr.instret = ctx.csr.instret.wrapping_add(1);
    StepResult::Ok
}

fn fma<B: SystemBus>(
    ctx: &mut ExecContext<B>,
    inst: Instruction,
    raw: u32,
    pc: u64,
) -> StepResult {
    if let Some(t) = check_fs(ctx, raw) {
        return t;
    }

    let rs1 = inst.rs1();
    let rs2 = inst.rs2();
    let rs3 = (raw >> 27) as usize;
    let rd = inst.rd();
    let fmt = (raw >> 25) & 3;

    match fmt {
        0 => {
            // single
            let a = f32::from_bits(ctx.regs.read_f32(rs1));
            let b = f32::from_bits(ctx.regs.read_f32(rs2));
            let c = f32::from_bits(ctx.regs.read_f32(rs3));
            let result = match inst.opcode() {
                OP_FMADD => a.mul_add(b, c),
                OP_FMSUB => a.mul_add(b, -c),
                OP_FNMSUB => (-a).mul_add(b, c),
                OP_FNMADD => (-a).mul_add(b, -c),
                _ => unreachable!(),
            };
            ctx.regs.write_f32(rd, result.to_bits());
        }
        1 => {
            // double
            let a = f64::from_bits(ctx.regs.read_f(rs1));
            let b = f64::from_bits(ctx.regs.read_f(rs2));
            let c = f64::from_bits(ctx.regs.read_f(rs3));
            let result = match inst.opcode() {
                OP_FMADD => a.mul_add(b, c),
                OP_FMSUB => a.mul_add(b, -c),
                OP_FNMSUB => (-a).mul_add(b, c),
                OP_FNMADD => (-a).mul_add(b, -c),
                _ => unreachable!(),
            };
            ctx.regs.write_f(rd, result.to_bits());
        }
        _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
    }

    mark_fs_dirty(ctx);
    ctx.regs.pc = pc.wrapping_add(4);
    ctx.csr.instret = ctx.csr.instret.wrapping_add(1);
    StepResult::Ok
}
