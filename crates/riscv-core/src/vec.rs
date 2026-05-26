//! RVV 1.0 vector execution for complexe calc

use crate::execute::{take_exception, ExecContext};
use crate::hart::{VLEN_BYTES, VREG_COUNT};
use crate::mmu::MmuFault;
use crate::system_bus::SystemBus;
use crate::trap::{StepResult, TrapCause};


pub const VLEN: u64 = 128; // bits
const ELEN: u64 = 64; // max element width (bits)


fn sew_bytes(vtype: u64) -> Option<u64> {
    let enc = vtype & 0x7;
    let bits: u64 = 8 << enc;
    if bits > ELEN { None } else { Some(bits / 8) }
}

fn vlmax(vtype: u64) -> Option<u64> {
    let sew_b = sew_bytes(vtype)?;
    let vlmul_enc = (vtype >> 3) & 0x7;
    let (num, den): (u64, u64) = match vlmul_enc {
        0 => (1, 1),
        5 => (1, 8), 6 => (1, 4), 7 => (1, 2),
        1 | 2 | 3 => return None,
        _ => return None,
    };
    let v = VLEN / 8 * num / den / sew_b;

    if v == 0 { None } else { Some(v) }
}

pub fn vsetvl_compute(avl: u64, vtype_val: u64) -> (u64, u64) {
    let Some(vm) = vlmax(vtype_val) else {
        return (0, 1u64 << 63);
    };
    let vl = if avl == u64::MAX { vm }
             else if avl <= vm  { avl }
             else { vm };
    (vl, vtype_val & 0xFF)
}


fn check_vs<B: SystemBus>(ctx: &ExecContext<B>, raw: u32) -> Option<StepResult> {
    // mstatus.VS
    if (ctx.csr.mstatus >> 9) & 3 == 0 {
        Some(StepResult::Trap(TrapCause::IllegalInstruction(raw)))
    } else {
        None
    }
}

fn mark_vs_dirty<B: SystemBus>(ctx: &mut ExecContext<B>) {
    ctx.csr.mstatus |= 3u64 << 9;
}

fn vreg_read_elem(vregs: &[[u8; VLEN_BYTES]; VREG_COUNT], reg: usize, elem: u64, sew_b: u64) -> u64 {
    let byte_off = (elem * sew_b) as usize;
    let src = &vregs[reg][byte_off..byte_off + sew_b as usize];
    match sew_b {
        1 => src[0] as u64,
        2 => u16::from_le_bytes(src.try_into().unwrap()) as u64,
        4 => u32::from_le_bytes(src.try_into().unwrap()) as u64,
        8 => u64::from_le_bytes(src.try_into().unwrap()),
        _ => unreachable!(),
    }
}

fn vreg_write_elem(vregs: &mut [[u8; VLEN_BYTES]; VREG_COUNT], reg: usize, elem: u64, sew_b: u64, val: u64) {
    let byte_off = (elem * sew_b) as usize;
    let dst = &mut vregs[reg][byte_off..byte_off + sew_b as usize];
    match sew_b {
        1 => dst[0] = val as u8,
        2 => dst.copy_from_slice(&(val as u16).to_le_bytes()),
        4 => dst.copy_from_slice(&(val as u32).to_le_bytes()),
        8 => dst.copy_from_slice(&val.to_le_bytes()),
        _ => unreachable!(),
    }
}

fn mask_bit(vregs: &[[u8; VLEN_BYTES]; VREG_COUNT], elem: u64) -> bool {
    let byte = (elem / 8) as usize;
    let bit  = (elem % 8) as u32;
    (vregs[0][byte] >> bit) & 1 != 0
}

fn effective_satp<B: SystemBus>(ctx: &ExecContext<B>) -> u64 {
    if *ctx.priv_mode == crate::csr::PrivMode::M { 0 } else { ctx.csr.satp }
}

fn vec_translate_load<B: SystemBus>(ctx: &mut ExecContext<B>, va: u64) -> Result<u64, MmuFault> {
    let satp = effective_satp(ctx);
    ctx.mmu.translate_load(va, satp, ctx.bus)
}

fn vec_translate_store<B: SystemBus>(ctx: &mut ExecContext<B>, va: u64) -> Result<u64, MmuFault> {
    let satp = effective_satp(ctx);
    ctx.mmu.translate_store(va, satp, ctx.bus)
}

// vsetvl*

fn exec_opcfg<B: SystemBus>(ctx: &mut ExecContext<B>, raw: u32, pc: u64) -> StepResult {
    let rd  = ((raw >> 7) & 0x1F) as usize;
    let rs1 = ((raw >> 15) & 0x1F) as usize;
    let bit31 = (raw >> 31) & 1;
    let bit30 = (raw >> 30) & 1;

    let (avl, vtype_val) = if bit31 == 0 {
        let zimm = ((raw >> 20) & 0x7FF) as u64;
        let avl = if rs1 == 0 && rd != 0 { u64::MAX }
                  else if rs1 == 0 { ctx.csr.vl }
                  else { ctx.regs.read(rs1) };
        (avl, zimm)
    } else if bit30 == 0 {
        let rs2 = ((raw >> 20) & 0x1F) as usize;
        let avl = if rs1 == 0 && rd != 0 { u64::MAX }
                  else if rs1 == 0 { ctx.csr.vl }
                  else { ctx.regs.read(rs1) };
        (avl, ctx.regs.read(rs2))
    } else {
        let zimm = ((raw >> 20) & 0x3FF) as u64;
        (rs1 as u64, zimm)
    };

    let (vl, vtype_out) = vsetvl_compute(avl, vtype_val);
    ctx.csr.vtype  = vtype_out;
    ctx.csr.vl     = vl;
    ctx.csr.vstart = 0;
    if rd != 0 { ctx.regs.write(rd, vl); }

    ctx.regs.pc = pc.wrapping_add(4);
    ctx.csr.instret = ctx.csr.instret.wrapping_add(1);
    StepResult::Ok
}

// unit-stride and strided load

fn exec_vload<B: SystemBus>(ctx: &mut ExecContext<B>, raw: u32, pc: u64) -> StepResult {
    if let Some(t) = check_vs(ctx, raw) { return t; }

    let vd   = ((raw >> 7)  & 0x1F) as usize;
    let rs1  = ((raw >> 15) & 0x1F) as usize;
    let rs2  = ((raw >> 20) & 0x1F) as usize;
    let vm   = (raw >> 25) & 1;     // 1 = unmasked
    let mop  = (raw >> 26) & 0x3;   // 0=unit 2=strided
    let nf   = (raw >> 29) & 0x7;   // segment count-1; we only support nf=0

    if nf != 0 { return StepResult::Trap(TrapCause::IllegalInstruction(raw)); }
    if mop != 0 && mop != 2 { return StepResult::Trap(TrapCause::IllegalInstruction(raw)); }

    let width_enc = (raw >> 12) & 0x7; // 0=8,5=16,6=32,7=64
    let sew_b: u64 = match width_enc {
        0 => 1, 5 => 2, 6 => 4, 7 => 8,
        _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
    };

    let base   = ctx.regs.read(rs1);
    let stride: i64 = if mop == 2 { ctx.regs.read(rs2) as i64 } else { sew_b as i64 };
    let vl = ctx.csr.vl;

    for i in 0..vl {
        if vm == 0 && !mask_bit(ctx.vregs, i) { continue; }
        let va = base.wrapping_add((stride * i as i64) as u64);
        let pa = match vec_translate_load(ctx, va) {
            Ok(p) => p,
            Err(f) => { take_exception(ctx, f.mcause(), f.tval()); return StepResult::Ok; }
        };
        let val = match sew_b {
            1 => ctx.bus.read_byte(pa) as u64,
            2 => ctx.bus.read_halfword(pa) as u64,
            4 => ctx.bus.read_word(pa) as u64,
            8 => ctx.bus.read_doubleword(pa),
            _ => unreachable!(),
        };
        vreg_write_elem(ctx.vregs, vd, i, sew_b, val);
    }

    mark_vs_dirty(ctx);
    ctx.csr.vstart = 0;
    ctx.regs.pc = pc.wrapping_add(4);
    ctx.csr.instret = ctx.csr.instret.wrapping_add(1);
    StepResult::Ok
}

// unit-stride and strided store

fn exec_vstore<B: SystemBus>(ctx: &mut ExecContext<B>, raw: u32, pc: u64) -> StepResult {
    if let Some(t) = check_vs(ctx, raw) { return t; }

    let vs3  = ((raw >> 7)  & 0x1F) as usize;
    let rs1  = ((raw >> 15) & 0x1F) as usize;
    let rs2  = ((raw >> 20) & 0x1F) as usize;
    let vm   = (raw >> 25) & 1;
    let mop  = (raw >> 26) & 0x3;
    let nf   = (raw >> 29) & 0x7;

    if nf != 0 { return StepResult::Trap(TrapCause::IllegalInstruction(raw)); }
    if mop != 0 && mop != 2 { return StepResult::Trap(TrapCause::IllegalInstruction(raw)); }

    let width_enc = (raw >> 12) & 0x7;
    let sew_b: u64 = match width_enc {
        0 => 1, 5 => 2, 6 => 4, 7 => 8,
        _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
    };

    let base   = ctx.regs.read(rs1);
    let stride: i64 = if mop == 2 { ctx.regs.read(rs2) as i64 } else { sew_b as i64 };
    let vl = ctx.csr.vl;

    for i in 0..vl {
        if vm == 0 && !mask_bit(ctx.vregs, i) { continue; }
        let val = vreg_read_elem(ctx.vregs, vs3, i, sew_b);
        let va = base.wrapping_add((stride * i as i64) as u64);
        let pa = match vec_translate_store(ctx, va) {
            Ok(p) => p,
            Err(f) => { take_exception(ctx, f.mcause(), f.tval()); return StepResult::Ok; }
        };
        match sew_b {
            1 => ctx.bus.write_byte(pa, val as u8),
            2 => ctx.bus.write_halfword(pa, val as u16),
            4 => ctx.bus.write_word(pa, val as u32),
            8 => ctx.bus.write_doubleword(pa, val),
            _ => unreachable!(),
        }
    }

    ctx.csr.vstart = 0;
    ctx.regs.pc = pc.wrapping_add(4);
    ctx.csr.instret = ctx.csr.instret.wrapping_add(1);
    StepResult::Ok
}

// OPI / OPM arithmetic dispatch
fn src_scalar(raw: u32, vv: bool, vx: bool,
              regs: &crate::gpr::Gpr,
              vregs: &[[u8; VLEN_BYTES]; VREG_COUNT],
              elem: u64, sew_b: u64) -> u64
{
    if vv {
        let vs1 = ((raw >> 15) & 0x1F) as usize;
        vreg_read_elem(vregs, vs1, elem, sew_b)
    } else if vx {
        let rs1 = ((raw >> 15) & 0x1F) as usize;
        regs.read(rs1)
    } else {
        let imm5 = ((raw >> 15) & 0x1F) as i64;
        ((imm5 << 59) >> 59) as u64
    }
}

fn exec_opivv_opivx_opivi<B: SystemBus>(
    ctx: &mut ExecContext<B>, raw: u32, pc: u64,
    funct6: u32, vv: bool, vx: bool,
) -> StepResult {
    if let Some(t) = check_vs(ctx, raw) { return t; }

    let vd  = ((raw >> 7)  & 0x1F) as usize;
    let vs2 = ((raw >> 20) & 0x1F) as usize;
    let vm  = (raw >> 25) & 1;

    let vtype = ctx.csr.vtype;
    let vl    = ctx.csr.vl;
    let Some(sew_b) = sew_bytes(vtype) else {
        return StepResult::Trap(TrapCause::IllegalInstruction(raw));
    };
    let sew_bits = sew_b * 8;
    let mask_bits = (1u64 << sew_bits).wrapping_sub(1);

    for i in 0..vl {
        if vm == 0 && !mask_bit(ctx.vregs, i) { continue; }

        let a = vreg_read_elem(ctx.vregs, vs2, i, sew_b);
        let b = src_scalar(raw, vv, vx, ctx.regs, ctx.vregs, i, sew_b);

        let result: u64 = match funct6 {
            0x00 => a.wrapping_add(b),
            0x02 => a.wrapping_sub(b),
            0x03 => b.wrapping_sub(a),
            0x09 => a & b,
            0x0A => a | b,
            0x0B => a ^ b,
            // shifts
            0x25 => a.wrapping_shl((b & (sew_bits - 1)) as u32),
            0x28 => (a & mask_bits).wrapping_shr((b & (sew_bits - 1)) as u32),
            0x29 => {
                let sa = sign_extend_sew(a, sew_bits);
                sa.wrapping_shr((b & (sew_bits - 1)) as u32) as u64
            }
            // vmv.v.*
            0x17 if vm == 1 => b,
            0x18 => {
                set_mask_bit(ctx.vregs, vd, i, a == b);
                continue;
            }
            0x19 => {
                set_mask_bit(ctx.vregs, vd, i, a != b);
                continue;
            }
            0x1A => {
                set_mask_bit(ctx.vregs, vd, i, a <  b);
                continue;
            }
            0x1B => {
                set_mask_bit(ctx.vregs, vd, i,
                        sign_extend_sew(a, sew_bits) < sign_extend_sew(b, sew_bits));
                continue;
            }
            0x1C => {
                set_mask_bit(ctx.vregs, vd, i, a <= b);
                continue;
            }
            0x1D => {
                set_mask_bit(ctx.vregs, vd, i,
                        sign_extend_sew(a, sew_bits) <= sign_extend_sew(b, sew_bits));
                continue;
            }
            0x1E => {
                set_mask_bit(ctx.vregs, vd, i, a >  b);
                continue;
            }
            0x1F => {
                set_mask_bit(ctx.vregs, vd, i,
                        sign_extend_sew(a, sew_bits) > sign_extend_sew(b, sew_bits));
                continue;
            }
            _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
        };

        vreg_write_elem(ctx.vregs, vd, i, sew_b, result & mask_bits);
    }

    mark_vs_dirty(ctx);
    ctx.csr.vstart = 0;
    ctx.regs.pc = pc.wrapping_add(4);
    ctx.csr.instret = ctx.csr.instret.wrapping_add(1);
    StepResult::Ok
}

fn exec_opmvv_opmvx<B: SystemBus>(
    ctx: &mut ExecContext<B>, raw: u32, pc: u64,
    funct6: u32, vv: bool,
) -> StepResult {
    if let Some(t) = check_vs(ctx, raw) { return t; }

    let vd  = ((raw >> 7)  & 0x1F) as usize;
    let vs2 = ((raw >> 20) & 0x1F) as usize;
    let vm  = (raw >> 25) & 1;

    let vtype = ctx.csr.vtype;
    let vl    = ctx.csr.vl;
    let Some(sew_b) = sew_bytes(vtype) else {
        return StepResult::Trap(TrapCause::IllegalInstruction(raw));
    };
    let sew_bits = sew_b * 8;
    let mask_bits = (1u64 << sew_bits).wrapping_sub(1);

    // vmv.x.s
    if funct6 == 0x10 && vv && ((raw >> 15) & 0x1F) == 0 {
        let val = vreg_read_elem(ctx.vregs, vs2, 0, sew_b);
        let xrd = ((raw >> 7) & 0x1F) as usize;
        if xrd != 0 { ctx.regs.write(xrd, sign_extend_sew(val, sew_bits) as u64); }
        ctx.regs.pc = pc.wrapping_add(4);
        ctx.csr.instret = ctx.csr.instret.wrapping_add(1);
        return StepResult::Ok;
    }
    // vmv.s.x
    if funct6 == 0x10 && !vv {
        if vl >= 1 {
            let rs1 = ((raw >> 15) & 0x1F) as usize;
            let val = ctx.regs.read(rs1) & mask_bits;
            vreg_write_elem(ctx.vregs, vd, 0, sew_b, val);
            mark_vs_dirty(ctx);
        }
        ctx.csr.vstart = 0;
        ctx.regs.pc = pc.wrapping_add(4);
        ctx.csr.instret = ctx.csr.instret.wrapping_add(1);
        return StepResult::Ok;
    }

    for i in 0..vl {
        if vm == 0 && !mask_bit(ctx.vregs, i) { continue; }

        let a = vreg_read_elem(ctx.vregs, vs2, i, sew_b);
        let b = if vv {
            let vs1 = ((raw >> 15) & 0x1F) as usize;
            vreg_read_elem(ctx.vregs, vs1, i, sew_b)
        } else {
            let rs1 = ((raw >> 15) & 0x1F) as usize;
            ctx.regs.read(rs1)
        };

        let result: u64 = match funct6 {
            0x20 => if b == 0 { mask_bits } else { a / (b & mask_bits) },  // vdivu
            0x21 => {
                let sa = sign_extend_sew(a, sew_bits);
                let sb = sign_extend_sew(b, sew_bits);
                if sb == 0 { mask_bits } else { sa.wrapping_div(sb) as u64 }
            }  // vdiv
            0x22 => if b == 0 { a } else { (a & mask_bits) % (b & mask_bits) }, // vremu
            0x23 => {
                let sa = sign_extend_sew(a, sew_bits);
                let sb = sign_extend_sew(b, sew_bits);
                if sb == 0 { a } else { sa.wrapping_rem(sb) as u64 }
            }  // vrem
            0x25 => a.wrapping_mul(b),
            0x26 => mulhu(a & mask_bits, b & mask_bits, sew_bits),
            0x27 => mulh(sign_extend_sew(a, sew_bits), sign_extend_sew(b, sew_bits), sew_bits),
            _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
        };

        vreg_write_elem(ctx.vregs, vd, i, sew_b, result & mask_bits);
    }

    mark_vs_dirty(ctx);
    ctx.csr.vstart = 0;
    ctx.regs.pc = pc.wrapping_add(4);
    ctx.csr.instret = ctx.csr.instret.wrapping_add(1);
    StepResult::Ok
}

fn exec_vredop<B: SystemBus>(
    ctx: &mut ExecContext<B>, raw: u32, pc: u64, funct6: u32,
) -> StepResult {
    if let Some(t) = check_vs(ctx, raw) { return t; }

    let vd  = ((raw >> 7)  & 0x1F) as usize;
    let vs1 = ((raw >> 15) & 0x1F) as usize;
    let vs2 = ((raw >> 20) & 0x1F) as usize;
    let vm  = (raw >> 25) & 1;

    let vtype = ctx.csr.vtype;
    let vl    = ctx.csr.vl;
    let Some(sew_b) = sew_bytes(vtype) else {
        return StepResult::Trap(TrapCause::IllegalInstruction(raw));
    };
    let sew_bits = sew_b * 8;
    let mask_bits = (1u64 << sew_bits).wrapping_sub(1);

    let mut acc = vreg_read_elem(ctx.vregs, vs1, 0, sew_b);

    for i in 0..vl {
        if vm == 0 && !mask_bit(ctx.vregs, i) { continue; }
        let a = vreg_read_elem(ctx.vregs, vs2, i, sew_b);
        acc = match funct6 {
            0x00 => acc.wrapping_add(a),
            0x01 => acc & a,
            0x02 => acc | a,
            0x03 => acc ^ a,
            _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
        };
    }
    vreg_write_elem(ctx.vregs, vd, 0, sew_b, acc & mask_bits);

    mark_vs_dirty(ctx);
    ctx.csr.vstart = 0;
    ctx.regs.pc = pc.wrapping_add(4);
    ctx.csr.instret = ctx.csr.instret.wrapping_add(1);
    StepResult::Ok
}

// mask logical ops

fn exec_mask_logical<B: SystemBus>(ctx: &mut ExecContext<B>, raw: u32, pc: u64, funct6: u32) -> StepResult {
    if let Some(t) = check_vs(ctx, raw) { return t; }

    let vd  = ((raw >> 7)  & 0x1F) as usize;
    let vs1 = ((raw >> 15) & 0x1F) as usize;
    let vs2 = ((raw >> 20) & 0x1F) as usize;

    let vl = ctx.csr.vl;
    for i in 0..vl {
        let byte = (i / 8) as usize;
        let bit  = (i % 8) as u32;
        let bit_vs2 = (ctx.vregs[vs2][byte] >> bit) & 1 != 0;
        let bit_vs1 = (ctx.vregs[vs1][byte] >> bit) & 1 != 0;

        let res = match funct6 {
            0x66 => bit_vs2 & bit_vs1,    // vmand
            0x67 => !(bit_vs2 & bit_vs1), // vmnand
            0x64 => bit_vs2 & !bit_vs1,   // vmandnot
            0x68 => bit_vs2 ^ bit_vs1,    // vmxor
            0x6A => bit_vs2 | bit_vs1,    // vmor
            0x6B => !(bit_vs2 | bit_vs1), // vmnor
            0x6C => !bit_vs2 | bit_vs1,   // vmornot (vs2 | ~vs1 per V spec §15.3)
            0x6E => !(bit_vs2 ^ bit_vs1), // vmxnor
            _ => return StepResult::Trap(TrapCause::IllegalInstruction(raw)),
        };
        set_mask_bit(ctx.vregs, vd, i, res);
    }

    mark_vs_dirty(ctx);
    ctx.csr.vstart = 0;
    ctx.regs.pc = pc.wrapping_add(4);
    ctx.csr.instret = ctx.csr.instret.wrapping_add(1);
    StepResult::Ok
}

// helpers

fn set_mask_bit(vregs: &mut [[u8; VLEN_BYTES]; VREG_COUNT], reg: usize, elem: u64, val: bool) {
    let byte = (elem / 8) as usize;
    let bit  = (elem % 8) as u32;
    if val {
        vregs[reg][byte] |=  (1 << bit);
    } else {
        vregs[reg][byte] &= !(1 << bit);
    }
}

fn sign_extend_sew(val: u64, sew_bits: u64) -> i64 {
    let shift = 64 - sew_bits;
    ((val as i64) << shift) >> shift
}

fn mulhu(a: u64, b: u64, sew_bits: u64) -> u64 {
    ((a as u128 * b as u128) >> sew_bits) as u64
}

fn mulh(a: i64, b: i64, sew_bits: u64) -> u64 {
    ((a as i128 * b as i128 >> sew_bits) as i64) as u64
}

// top-level dispatcher

pub fn exec_vec<B: SystemBus>(ctx: &mut ExecContext<B>, raw: u32, pc: u64) -> StepResult {
    let opcode  = raw & 0x7F;
    let funct3  = (raw >> 12) & 0x7;
    let funct6  = raw >> 26;

    if opcode == 0x07 {
        let width = (raw >> 12) & 0x7;
        if matches!(width, 0 | 5 | 6 | 7) {
            return exec_vload(ctx, raw, pc);
        }
        return StepResult::Trap(TrapCause::IllegalInstruction(raw));
    }

    if opcode == 0x27 {
        let width = (raw >> 12) & 0x7;
        if matches!(width, 0 | 5 | 6 | 7) {
            return exec_vstore(ctx, raw, pc);
        }
        return StepResult::Trap(TrapCause::IllegalInstruction(raw));
    }

    // OP_VEC (0x57)
    match funct3 {
        7 => exec_opcfg(ctx, raw, pc),   // vsetvl*

        // OPIVV=0, OPIVX=4, OPIVI=3
        0 => {
            match funct6 {
                0x64 | 0x66 | 0x67 | 0x68 | 0x6A | 0x6B | 0x6C | 0x6E
                    => exec_mask_logical(ctx, raw, pc, funct6),
                _ => exec_opivv_opivx_opivi(ctx, raw, pc, funct6, true, false),
            }
        }
        3 => exec_opivv_opivx_opivi(ctx, raw, pc, funct6, false, false), // OPIVI
        4 => exec_opivv_opivx_opivi(ctx, raw, pc, funct6, false, true),  // OPIVX

        // OPMVV=2, OPMVX=6
        2 => {
            match funct6 {
                0x00 | 0x01 | 0x02 | 0x03 => exec_vredop(ctx, raw, pc, funct6),
                _ => exec_opmvv_opmvx(ctx, raw, pc, funct6, true),
            }
        }
        6 => exec_opmvv_opmvx(ctx, raw, pc, funct6, false),

        // OPFVV=1, OPFVF=5
        _ => StepResult::Trap(TrapCause::IllegalInstruction(raw)),
    }
}
