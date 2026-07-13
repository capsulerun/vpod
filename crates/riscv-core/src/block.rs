use std::sync::Arc;

use crate::csr::PrivMode;
use crate::decode::{CompressedInstruction, Instruction, sign_extend};
use crate::execute::{ExecContext, take_exception};
use crate::extensions as ext;
use crate::mmu::{Mmu, MmuFault};
use crate::system_bus::SystemBus;

pub const MAX_BLOCK_OPS: usize = 64;

const CACHE_SLOTS: usize = 16384;

#[derive(Clone, Copy, Debug)]
pub enum AluKind {
    Add,
    Sub,
    Sll,
    Slt,
    Sltu,
    Xor,
    Srl,
    Sra,
    Or,
    And,
    Addw,
    Subw,
    Sllw,
    Srlw,
    Sraw,
    Mul,
    Mulh,
    Mulhsu,
    Mulhu,
    Div,
    Divu,
    Rem,
    Remu,
    Mulw,
    Divw,
    Divuw,
    Remw,
    Remuw,
    Andn,
    Orn,
    Xnor,
    Rol,
    Ror,
    Rolw,
    Rorw,
    Min,
    Max,
    Minu,
    Maxu,
    Clz,
    Ctz,
    Cpop,
    Clzw,
    Ctzw,
    Cpopw,
    SextB,
    SextH,
    ZextH,
    Rev8,
    OrcB,
}

#[derive(Clone, Copy, Debug)]
pub enum LoadKind {
    Lb,
    Lbu,
    Lh,
    Lhu,
    Lw,
    Lwu,
    Ld,
}

#[derive(Clone, Copy, Debug)]
pub enum StoreKind {
    Sb,
    Sh,
    Sw,
    Sd,
}

#[derive(Clone, Copy, Debug)]
pub enum BranchKind {
    Beq,
    Bne,
    Blt,
    Bge,
    Bltu,
    Bgeu,
}

#[derive(Clone, Copy, Debug)]
pub enum Op {
    Lui {
        rd: u8,
        imm: i64,
    },
    Auipc {
        rd: u8,
        imm: i64,
    },
    AluImm {
        kind: AluKind,
        rd: u8,
        rs1: u8,
        imm: i64,
    },
    AluReg {
        kind: AluKind,
        rd: u8,
        rs1: u8,
        rs2: u8,
    },
    Load {
        kind: LoadKind,
        rd: u8,
        rs1: u8,
        imm: i64,
    },
    Store {
        kind: StoreKind,
        rs1: u8,
        rs2: u8,
        imm: i64,
    },
    Branch {
        kind: BranchKind,
        rs1: u8,
        rs2: u8,
        offset: i64,
    },
    Jal {
        rd: u8,
        offset: i64,
    },
    Jalr {
        rd: u8,
        rs1: u8,
        imm: i64,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct DecodedInsn {
    pub op: Op,
    pub pc_off: u16,
    pub ilen: u8,
}

pub struct Block {
    pub ops: Box<[DecodedInsn]>,
    pub byte_len: u32,
}

struct Slot {
    tag: u64,
    block: Option<Arc<Block>>,
}

impl Slot {
    const EMPTY: Self = Self {
        tag: u64::MAX,
        block: None,
    };
}

pub struct BlockCache {
    slots: Vec<Slot>,
    page_bitmap: Vec<u64>,
}

impl Default for BlockCache {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockCache {
    pub fn new() -> Self {
        let mut slots = Vec::with_capacity(CACHE_SLOTS);

        for _ in 0..CACHE_SLOTS {
            slots.push(Slot::EMPTY);
        }

        Self {
            slots,
            page_bitmap: Vec::new(),
        }
    }

    #[inline(always)]
    fn slot_index(physical_address: u64) -> usize {
        ((physical_address >> 1) as usize) & (CACHE_SLOTS - 1)
    }

    #[inline(always)]
    pub fn lookup(&self, physical_address: u64) -> Option<Arc<Block>> {
        let slot = &self.slots[Self::slot_index(physical_address)];

        if slot.tag == physical_address {
            slot.block.clone()
        } else {
            None
        }
    }

    pub fn insert(&mut self, physical_address: u64, block: Block) -> Arc<Block> {
        let shared_block = Arc::new(block);
        self.mark_page(physical_address >> 12);

        let slot = &mut self.slots[Self::slot_index(physical_address)];
        *slot = Slot::EMPTY;
        slot.tag = physical_address;
        slot.block = Some(shared_block.clone());

        shared_block
    }

    fn mark_page(&mut self, page: u64) {
        let word = (page / 64) as usize;

        if word >= self.page_bitmap.len() {
            self.page_bitmap.resize(word + 1, 0);
        }

        self.page_bitmap[word] |= 1 << (page % 64);
    }

    #[inline(always)]
    pub fn notify_store(&mut self, physical_address: u64) {
        let page = physical_address >> 12;
        let word = (page / 64) as usize;

        if word < self.page_bitmap.len() && self.page_bitmap[word] & (1 << (page % 64)) != 0 {
            self.evict_page(page);
        }
    }

    fn evict_page(&mut self, page: u64) {
        for slot in &mut self.slots {
            if slot.tag != u64::MAX && slot.tag >> 12 == page {
                *slot = Slot::EMPTY;
            }
        }

        self.page_bitmap[(page / 64) as usize] &= !(1 << (page % 64));
    }

    pub fn flush_all(&mut self) {
        for slot in &mut self.slots {
            *slot = Slot::EMPTY;
        }

        self.page_bitmap.clear();
    }
}

pub fn decode_block<B: SystemBus>(bus: &mut B, physical_address: u64) -> Option<Block> {
    let mut ops: Vec<DecodedInsn> = Vec::new();
    let mut offset_in_block: u64 = 0;
    let page_end = 0x1000 - (physical_address & 0xfff);

    while ops.len() < MAX_BLOCK_OPS && offset_in_block + 2 <= page_end {
        let low_halfword = bus.read_halfword(physical_address + offset_in_block) as u32;

        let (op, ilen) = if low_halfword & 0x3 != 0x3 {
            match decode_compressed(low_halfword as u16) {
                Some(op) => (op, 2u8),
                None => break,
            }
        } else {
            if offset_in_block + 4 > page_end {
                break;
            }

            let high_halfword = bus.read_halfword(physical_address + offset_in_block + 2) as u32;

            match decode_full(low_halfword | (high_halfword << 16)) {
                Some(op) => (op, 4u8),
                None => break,
            }
        };

        let terminator = matches!(op, Op::Branch { .. } | Op::Jal { .. } | Op::Jalr { .. });
        ops.push(DecodedInsn {
            op,
            pc_off: offset_in_block as u16,
            ilen,
        });
        offset_in_block += ilen as u64;

        if terminator {
            break;
        }
    }

    if ops.is_empty() {
        return None;
    }

    Some(Block {
        ops: ops.into_boxed_slice(),
        byte_len: offset_in_block as u32,
    })
}

fn decode_full(raw: u32) -> Option<Op> {
    let inst = Instruction(raw);
    let rd = inst.rd() as u8;
    let rs1 = inst.rs1() as u8;
    let rs2 = inst.rs2() as u8;
    let funct3 = inst.funct3();
    let funct7 = inst.funct7();

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

    match inst.opcode() {
        OP_LUI => Some(Op::Lui {
            rd,
            imm: inst.imm_u(),
        }),
        OP_AUIPC => Some(Op::Auipc {
            rd,
            imm: inst.imm_u(),
        }),
        OP_JAL => Some(Op::Jal {
            rd,
            offset: inst.imm_j(),
        }),
        OP_JALR if funct3 == 0 => Some(Op::Jalr {
            rd,
            rs1,
            imm: inst.imm_i(),
        }),
        OP_BRANCH => {
            let kind = match funct3 {
                0x0 => BranchKind::Beq,
                0x1 => BranchKind::Bne,
                0x4 => BranchKind::Blt,
                0x5 => BranchKind::Bge,
                0x6 => BranchKind::Bltu,
                0x7 => BranchKind::Bgeu,
                _ => return None,
            };
            Some(Op::Branch {
                kind,
                rs1,
                rs2,
                offset: inst.imm_b(),
            })
        }
        OP_LOAD => {
            let kind = match funct3 {
                0x0 => LoadKind::Lb,
                0x1 => LoadKind::Lh,
                0x2 => LoadKind::Lw,
                0x3 => LoadKind::Ld,
                0x4 => LoadKind::Lbu,
                0x5 => LoadKind::Lhu,
                0x6 => LoadKind::Lwu,
                _ => return None,
            };
            Some(Op::Load {
                kind,
                rd,
                rs1,
                imm: inst.imm_i(),
            })
        }
        OP_STORE => {
            let kind = match funct3 {
                0x0 => StoreKind::Sb,
                0x1 => StoreKind::Sh,
                0x2 => StoreKind::Sw,
                0x3 => StoreKind::Sd,
                _ => return None,
            };
            Some(Op::Store {
                kind,
                rs1,
                rs2,
                imm: inst.imm_s(),
            })
        }
        OP_IMM => {
            let imm = inst.imm_i();
            let shamt = imm & 0x3f;

            let kind_imm = match funct3 {
                0x0 => Some((AluKind::Add, imm)),
                0x1 => match funct7 >> 1 {
                    0x00 => Some((AluKind::Sll, shamt)),
                    0x18 => match shamt {
                        0 => Some((AluKind::Clz, 0)),
                        1 => Some((AluKind::Ctz, 0)),
                        2 => Some((AluKind::Cpop, 0)),
                        _ => None,
                    },
                    0x1a => match shamt {
                        2 => Some((AluKind::SextB, 0)),
                        4 => Some((AluKind::SextH, 0)),
                        _ => None,
                    },
                    _ => None,
                },
                0x2 => Some((AluKind::Slt, imm)),
                0x3 => Some((AluKind::Sltu, imm)),
                0x4 => Some((AluKind::Xor, imm)),
                0x5 => match funct7 >> 1 {
                    0x00 => Some((AluKind::Srl, shamt)),
                    0x10 => Some((AluKind::Sra, shamt)),
                    0x14 if shamt == 7 => Some((AluKind::OrcB, 0)),
                    0x35 if shamt == 24 || shamt == 8 => Some((AluKind::Rev8, 0)),
                    0x18 => Some((AluKind::Ror, shamt)),
                    _ => None,
                },
                0x6 => Some((AluKind::Or, imm)),
                0x7 => Some((AluKind::And, imm)),
                _ => None,
            };
            kind_imm.map(|(kind, imm)| Op::AluImm { kind, rd, rs1, imm })
        }
        OP_IMM32 => {
            let imm = inst.imm_i();
            let shamt = imm & 0x1f;

            let kind_imm = match funct3 {
                0x0 => Some((AluKind::Addw, imm)),
                0x1 => match funct7 {
                    0x00 => Some((AluKind::Sllw, shamt)),
                    0x30 => match shamt {
                        0 => Some((AluKind::Clzw, 0)),
                        1 => Some((AluKind::Ctzw, 0)),
                        2 => Some((AluKind::Cpopw, 0)),
                        _ => None,
                    },
                    _ => None,
                },
                0x5 => match funct7 {
                    0x00 => Some((AluKind::Srlw, shamt)),
                    0x20 => Some((AluKind::Sraw, shamt)),
                    0x30 => Some((AluKind::Rorw, shamt)),
                    _ => None,
                },
                _ => None,
            };
            kind_imm.map(|(kind, imm)| Op::AluImm { kind, rd, rs1, imm })
        }
        OP_REG => {
            let kind = match (funct3, funct7) {
                (0x0, 0x00) => AluKind::Add,
                (0x0, 0x20) => AluKind::Sub,
                (0x1, 0x00) => AluKind::Sll,
                (0x2, 0x00) => AluKind::Slt,
                (0x3, 0x00) => AluKind::Sltu,
                (0x4, 0x00) => AluKind::Xor,
                (0x5, 0x00) => AluKind::Srl,
                (0x5, 0x20) => AluKind::Sra,
                (0x6, 0x00) => AluKind::Or,
                (0x7, 0x00) => AluKind::And,
                (0x0, 0x01) => AluKind::Mul,
                (0x1, 0x01) => AluKind::Mulh,
                (0x2, 0x01) => AluKind::Mulhsu,
                (0x3, 0x01) => AluKind::Mulhu,
                (0x4, 0x01) => AluKind::Div,
                (0x5, 0x01) => AluKind::Divu,
                (0x6, 0x01) => AluKind::Rem,
                (0x7, 0x01) => AluKind::Remu,
                (0x4, 0x20) => AluKind::Xnor,
                (0x6, 0x20) => AluKind::Orn,
                (0x7, 0x20) => AluKind::Andn,
                (0x1, 0x30) => AluKind::Rol,
                (0x5, 0x30) => AluKind::Ror,
                (0x4, 0x05) => AluKind::Min,
                (0x5, 0x05) => AluKind::Max,
                (0x6, 0x05) => AluKind::Minu,
                (0x7, 0x05) => AluKind::Maxu,
                (0x4, 0x04) => AluKind::ZextH,
                _ => return None,
            };
            Some(Op::AluReg { kind, rd, rs1, rs2 })
        }
        OP_REG32 => {
            let kind = match (funct3, funct7) {
                (0x0, 0x00) => AluKind::Addw,
                (0x0, 0x20) => AluKind::Subw,
                (0x1, 0x00) => AluKind::Sllw,
                (0x5, 0x00) => AluKind::Srlw,
                (0x5, 0x20) => AluKind::Sraw,
                (0x0, 0x01) => AluKind::Mulw,
                (0x4, 0x01) => AluKind::Divw,
                (0x5, 0x01) => AluKind::Divuw,
                (0x6, 0x01) => AluKind::Remw,
                (0x7, 0x01) => AluKind::Remuw,
                (0x1, 0x30) => AluKind::Rolw,
                (0x5, 0x30) => AluKind::Rorw,
                _ => return None,
            };

            Some(Op::AluReg { kind, rd, rs1, rs2 })
        }
        _ => None,
    }
}

fn decode_compressed(raw: u16) -> Option<Op> {
    let c = CompressedInstruction(raw);
    let quad = c.quadrant();
    let funct = c.funct3() as u32;

    match (quad, funct) {
        (0, 0x0) => {
            // C.ADDI4SPN
            let nzuimm = ((((raw >> 6) & 1) << 2)
                | (((raw >> 5) & 1) << 3)
                | (((raw >> 11) & 0x3) << 4)
                | (((raw >> 7) & 0xf) << 6)) as i64;

            if nzuimm == 0 {
                return None;
            }

            Some(Op::AluImm {
                kind: AluKind::Add,
                rd: c.rs2_prime() as u8,
                rs1: 2,
                imm: nzuimm,
            })
        }
        (0, 0x2) => {
            // C.LW
            let uimm = ((((raw >> 6) & 1) << 2)
                | (((raw >> 10) & 0x7) << 3)
                | (((raw >> 5) & 1) << 6)) as i64;

            Some(Op::Load {
                kind: LoadKind::Lw,
                rd: c.rs2_prime() as u8,
                rs1: c.rs1_prime() as u8,
                imm: uimm,
            })
        }
        (0, 0x3) => {
            // C.LD
            let uimm = ((((raw >> 10) & 0x7) << 3) | (((raw >> 5) & 0x3) << 6)) as i64;

            Some(Op::Load {
                kind: LoadKind::Ld,
                rd: c.rs2_prime() as u8,
                rs1: c.rs1_prime() as u8,
                imm: uimm,
            })
        }
        (0, 0x6) => {
            // C.SW
            let uimm = ((((raw >> 6) & 1) << 2)
                | (((raw >> 10) & 0x7) << 3)
                | (((raw >> 5) & 1) << 6)) as i64;

            Some(Op::Store {
                kind: StoreKind::Sw,
                rs1: c.rs1_prime() as u8,
                rs2: c.rs2_prime() as u8,
                imm: uimm,
            })
        }
        (0, 0x7) => {
            // C.SD
            let uimm = ((((raw >> 10) & 0x7) << 3) | (((raw >> 5) & 0x3) << 6)) as i64;

            Some(Op::Store {
                kind: StoreKind::Sd,
                rs1: c.rs1_prime() as u8,
                rs2: c.rs2_prime() as u8,
                imm: uimm,
            })
        }
        (1, 0x0) => {
            // C.ADDI
            let imm = sign_extend(((((raw >> 12) & 1) << 5) | ((raw >> 2) & 0x1f)) as i64, 6);
            let rd = c.rd() as u8;

            Some(Op::AluImm {
                kind: AluKind::Add,
                rd,
                rs1: rd,
                imm,
            })
        }
        (1, 0x1) => {
            // C.ADDIW
            let imm = sign_extend(((((raw >> 12) & 1) << 5) | ((raw >> 2) & 0x1f)) as i64, 6);
            let rd = c.rd() as u8;

            Some(Op::AluImm {
                kind: AluKind::Addw,
                rd,
                rs1: rd,
                imm,
            })
        }
        (1, 0x2) => {
            // C.LI
            let imm = sign_extend(((((raw >> 12) & 1) << 5) | ((raw >> 2) & 0x1f)) as i64, 6);

            Some(Op::AluImm {
                kind: AluKind::Add,
                rd: c.rd() as u8,
                rs1: 0,
                imm,
            })
        }
        (1, 0x3) => {
            let rd = c.rd() as u8;
            if rd == 2 {
                // C.ADDI16SP
                let imm = sign_extend(
                    ((((raw >> 12) & 1) << 9)
                        | (((raw >> 6) & 1) << 4)
                        | (((raw >> 5) & 1) << 6)
                        | (((raw >> 3) & 3) << 7)
                        | (((raw >> 2) & 1) << 5)) as i64,
                    10,
                );

                Some(Op::AluImm {
                    kind: AluKind::Add,
                    rd: 2,
                    rs1: 2,
                    imm,
                })
            } else {
                // C.LUI
                let r32 = raw as u32;
                let imm = sign_extend(
                    ((((r32 >> 12) & 1) << 17) | (((r32 >> 2) & 0x1f) << 12)) as i64,
                    18,
                );

                Some(Op::Lui { rd, imm })
            }
        }
        (1, 0x4) => {
            let rd = c.rs1_prime() as u8;
            let rs2 = c.rs2_prime() as u8;
            let shamt = ((((raw >> 12) & 1) << 5) | ((raw >> 2) & 0x1f)) as i64;

            match (raw >> 10) & 0x3 {
                0x0 => Some(Op::AluImm {
                    kind: AluKind::Srl,
                    rd,
                    rs1: rd,
                    imm: shamt,
                }),
                0x1 => Some(Op::AluImm {
                    kind: AluKind::Sra,
                    rd,
                    rs1: rd,
                    imm: shamt,
                }),
                0x2 => {
                    let imm =
                        sign_extend(((((raw >> 12) & 1) << 5) | ((raw >> 2) & 0x1f)) as i64, 6);
                    Some(Op::AluImm {
                        kind: AluKind::And,
                        rd,
                        rs1: rd,
                        imm,
                    })
                }
                0x3 => {
                    let kind = match ((raw >> 12) & 1, (raw >> 5) & 0x3) {
                        (0, 0x0) => AluKind::Sub,
                        (0, 0x1) => AluKind::Xor,
                        (0, 0x2) => AluKind::Or,
                        (0, 0x3) => AluKind::And,
                        (1, 0x0) => AluKind::Subw,
                        (1, 0x1) => AluKind::Addw,
                        _ => return None,
                    };
                    Some(Op::AluReg {
                        kind,
                        rd,
                        rs1: rd,
                        rs2,
                    })
                }
                _ => unreachable!(),
            }
        }
        (1, 0x5) => {
            // C.J
            let r = raw as u32;
            let val = ((((r >> 12) & 1) << 11)
                | (((r >> 11) & 1) << 4)
                | (((r >> 9) & 3) << 8)
                | (((r >> 8) & 1) << 10)
                | (((r >> 7) & 1) << 6)
                | (((r >> 6) & 1) << 7)
                | (((r >> 3) & 7) << 1)
                | (((r >> 2) & 1) << 5)) as i64;

            Some(Op::Jal {
                rd: 0,
                offset: sign_extend(val, 12),
            })
        }
        (1, 0x6) | (1, 0x7) => {
            // C.BEQZ / C.BNEZ
            let r = raw as u32;
            let val = ((((r >> 12) & 1) << 8)
                | (((r >> 10) & 3) << 3)
                | (((r >> 5) & 3) << 6)
                | (((r >> 3) & 3) << 1)
                | (((r >> 2) & 1) << 5)) as i64;

            let kind = if funct == 0x6 {
                BranchKind::Beq
            } else {
                BranchKind::Bne
            };

            Some(Op::Branch {
                kind,
                rs1: c.rs1_prime() as u8,
                rs2: 0,
                offset: sign_extend(val, 9),
            })
        }
        (2, 0x0) => {
            // C.SLLI
            let shamt = ((((raw >> 12) & 1) << 5) | ((raw >> 2) & 0x1f)) as i64;
            let rd = c.rd() as u8;

            Some(Op::AluImm {
                kind: AluKind::Sll,
                rd,
                rs1: rd,
                imm: shamt,
            })
        }
        (2, 0x2) => {
            // C.LWSP
            let uimm = ((((raw >> 12) & 1) << 5)
                | (((raw >> 4) & 0x7) << 2)
                | (((raw >> 2) & 0x3) << 6)) as i64;

            Some(Op::Load {
                kind: LoadKind::Lw,
                rd: c.rd() as u8,
                rs1: 2,
                imm: uimm,
            })
        }
        (2, 0x3) => {
            // C.LDSP
            let uimm = ((((raw >> 12) & 1) << 5)
                | (((raw >> 5) & 0x3) << 3)
                | (((raw >> 2) & 0x7) << 6)) as i64;

            Some(Op::Load {
                kind: LoadKind::Ld,
                rd: c.rd() as u8,
                rs1: 2,
                imm: uimm,
            })
        }
        (2, 0x4) => {
            let rd = c.rd() as u8;
            let rs2 = c.rs2() as u8;

            if (raw >> 12) & 1 == 0 {
                if rs2 == 0 {
                    // C.JR
                    Some(Op::Jalr {
                        rd: 0,
                        rs1: rd,
                        imm: 0,
                    })
                } else {
                    // C.MV
                    Some(Op::AluReg {
                        kind: AluKind::Add,
                        rd,
                        rs1: 0,
                        rs2,
                    })
                }
            } else if rd == 0 && rs2 == 0 {
                None // C.EBREAK
            } else if rs2 == 0 {
                // C.JALR
                Some(Op::Jalr {
                    rd: 1,
                    rs1: rd,
                    imm: 0,
                })
            } else {
                // C.ADD
                Some(Op::AluReg {
                    kind: AluKind::Add,
                    rd,
                    rs1: rd,
                    rs2,
                })
            }
        }
        (2, 0x6) => {
            // C.SWSP
            let uimm = ((((raw >> 9) & 0xf) << 2) | (((raw >> 7) & 0x3) << 6)) as i64;

            Some(Op::Store {
                kind: StoreKind::Sw,
                rs1: 2,
                rs2: c.rs2() as u8,
                imm: uimm,
            })
        }
        (2, 0x7) => {
            // C.SDSP
            let uimm = ((((raw >> 10) & 0x7) << 3) | (((raw >> 7) & 0x7) << 6)) as i64;

            Some(Op::Store {
                kind: StoreKind::Sd,
                rs1: 2,
                rs2: c.rs2() as u8,
                imm: uimm,
            })
        }
        _ => None,
    }
}

#[inline(always)]
fn alu(kind: AluKind, lhs: u64, rhs: u64) -> u64 {
    match kind {
        AluKind::Add => lhs.wrapping_add(rhs),
        AluKind::Sub => lhs.wrapping_sub(rhs),
        AluKind::Sll => lhs << (rhs & 0x3f),
        AluKind::Slt => ((lhs as i64) < (rhs as i64)) as u64,
        AluKind::Sltu => (lhs < rhs) as u64,
        AluKind::Xor => lhs ^ rhs,
        AluKind::Srl => lhs >> (rhs & 0x3f),
        AluKind::Sra => ((lhs as i64) >> (rhs & 0x3f)) as u64,
        AluKind::Or => lhs | rhs,
        AluKind::And => lhs & rhs,
        AluKind::Addw => (lhs as u32).wrapping_add(rhs as u32) as i32 as i64 as u64,
        AluKind::Subw => (lhs as u32).wrapping_sub(rhs as u32) as i32 as i64 as u64,
        AluKind::Sllw => ((lhs as u32) << (rhs & 0x1f)) as i32 as i64 as u64,
        AluKind::Srlw => ((lhs as u32) >> (rhs & 0x1f)) as i32 as i64 as u64,
        AluKind::Sraw => ((lhs as i32) >> (rhs & 0x1f)) as i64 as u64,
        AluKind::Mul => ext::mul(lhs, rhs),
        AluKind::Mulh => ext::mulh(lhs, rhs),
        AluKind::Mulhsu => ext::mulhsu(lhs, rhs),
        AluKind::Mulhu => ext::mulhu(lhs, rhs),
        AluKind::Div => ext::div(lhs, rhs),
        AluKind::Divu => ext::divu(lhs, rhs),
        AluKind::Rem => ext::rem(lhs, rhs),
        AluKind::Remu => ext::remu(lhs, rhs),
        AluKind::Mulw => ext::mulw(lhs, rhs),
        AluKind::Divw => ext::divw(lhs, rhs),
        AluKind::Divuw => ext::divuw(lhs, rhs),
        AluKind::Remw => ext::remw(lhs, rhs),
        AluKind::Remuw => ext::remuw(lhs, rhs),
        AluKind::Andn => lhs & !rhs,
        AluKind::Orn => lhs | !rhs,
        AluKind::Xnor => lhs ^ !rhs,
        AluKind::Rol => lhs.rotate_left((rhs & 0x3f) as u32),
        AluKind::Ror => lhs.rotate_right((rhs & 0x3f) as u32),
        AluKind::Rolw => ((lhs as u32).rotate_left((rhs & 0x1f) as u32)) as i32 as i64 as u64,
        AluKind::Rorw => ((lhs as u32).rotate_right((rhs & 0x1f) as u32)) as i32 as i64 as u64,
        AluKind::Min => {
            if (lhs as i64) < (rhs as i64) {
                lhs
            } else {
                rhs
            }
        }
        AluKind::Max => {
            if (lhs as i64) > (rhs as i64) {
                lhs
            } else {
                rhs
            }
        }
        AluKind::Minu => lhs.min(rhs),
        AluKind::Maxu => lhs.max(rhs),
        AluKind::Clz => lhs.leading_zeros() as u64,
        AluKind::Ctz => lhs.trailing_zeros() as u64,
        AluKind::Cpop => lhs.count_ones() as u64,
        AluKind::Clzw => (lhs as u32).leading_zeros() as u64,
        AluKind::Ctzw => (lhs as u32).trailing_zeros() as u64,
        AluKind::Cpopw => (lhs as u32).count_ones() as u64,
        AluKind::SextB => lhs as i8 as i64 as u64,
        AluKind::SextH => lhs as i16 as i64 as u64,
        AluKind::ZextH => lhs as u16 as u64,
        AluKind::Rev8 => lhs.swap_bytes(),
        AluKind::OrcB => {
            let mut result = 0u64;
            for i in 0..8 {
                let byte = (lhs >> (i * 8)) & 0xFF;
                let out = if byte != 0 { 0xFF } else { 0x00 };
                result |= out << (i * 8);
            }
            result
        }
    }
}

pub fn exec_block<B: SystemBus>(
    ctx: &mut ExecContext<B>,
    block: &Block,
    entry_pc: u64,
    satp: u64,
) -> u64 {
    let mut retired_instructions: u64 = 0;

    for decoded_instruction in block.ops.iter() {
        let pc = entry_pc.wrapping_add(decoded_instruction.pc_off as u64);

        match decoded_instruction.op {
            Op::Lui { rd, imm } => {
                ctx.regs.write(rd as usize, imm as u64);
            }
            Op::Auipc { rd, imm } => {
                ctx.regs.write(rd as usize, pc.wrapping_add(imm as u64));
            }
            Op::AluImm { kind, rd, rs1, imm } => {
                let a = ctx.regs.read(rs1 as usize);
                ctx.regs.write(rd as usize, alu(kind, a, imm as u64));
            }
            Op::AluReg { kind, rd, rs1, rs2 } => {
                let a = ctx.regs.read(rs1 as usize);
                let b = ctx.regs.read(rs2 as usize);

                ctx.regs.write(rd as usize, alu(kind, a, b));
            }
            Op::Load { kind, rd, rs1, imm } => {
                let virtual_address = ctx.regs.read(rs1 as usize).wrapping_add(imm as u64);

                ctx.regs.pc = pc;
                match do_load(ctx, satp, kind, virtual_address) {
                    Ok(v) => ctx.regs.write(rd as usize, v),
                    Err(()) => {
                        ctx.csr.instret = ctx.csr.instret.wrapping_add(retired_instructions);
                        return retired_instructions + 1;
                    }
                }
            }
            Op::Store {
                kind,
                rs1,
                rs2,
                imm,
            } => {
                let virtual_address = ctx.regs.read(rs1 as usize).wrapping_add(imm as u64);
                let val = ctx.regs.read(rs2 as usize);
                ctx.regs.pc = pc;
                if do_store(ctx, satp, kind, virtual_address, val).is_err() {
                    ctx.csr.instret = ctx.csr.instret.wrapping_add(retired_instructions);
                    return retired_instructions + 1;
                }
            }
            Op::Branch {
                kind,
                rs1,
                rs2,
                offset,
            } => {
                let a = ctx.regs.read(rs1 as usize);
                let b = ctx.regs.read(rs2 as usize);
                let taken = match kind {
                    BranchKind::Beq => a == b,
                    BranchKind::Bne => a != b,
                    BranchKind::Blt => (a as i64) < (b as i64),
                    BranchKind::Bge => (a as i64) >= (b as i64),
                    BranchKind::Bltu => a < b,
                    BranchKind::Bgeu => a >= b,
                };

                ctx.regs.pc = if taken {
                    pc.wrapping_add(offset as u64)
                } else {
                    pc.wrapping_add(decoded_instruction.ilen as u64)
                };

                retired_instructions += 1;
                ctx.csr.instret = ctx.csr.instret.wrapping_add(retired_instructions);

                return retired_instructions;
            }
            Op::Jal { rd, offset } => {
                ctx.regs.write(
                    rd as usize,
                    pc.wrapping_add(decoded_instruction.ilen as u64),
                );

                ctx.regs.pc = pc.wrapping_add(offset as u64);
                retired_instructions += 1;
                ctx.csr.instret = ctx.csr.instret.wrapping_add(retired_instructions);

                return retired_instructions;
            }
            Op::Jalr { rd, rs1, imm } => {
                let target = ctx.regs.read(rs1 as usize).wrapping_add(imm as u64) & !1;
                ctx.regs.write(
                    rd as usize,
                    pc.wrapping_add(decoded_instruction.ilen as u64),
                );

                ctx.regs.pc = target;
                retired_instructions += 1;
                ctx.csr.instret = ctx.csr.instret.wrapping_add(retired_instructions);

                return retired_instructions;
            }
        }

        retired_instructions += 1;
    }

    ctx.regs.pc = entry_pc.wrapping_add(block.byte_len as u64);
    ctx.csr.instret = ctx.csr.instret.wrapping_add(retired_instructions);
    retired_instructions
}

fn do_load<B: SystemBus>(
    ctx: &mut ExecContext<B>,
    satp: u64,
    kind: LoadKind,
    virtual_address: u64,
) -> Result<u64, ()> {
    let size: u64 = match kind {
        LoadKind::Lb | LoadKind::Lbu => 1,
        LoadKind::Lh | LoadKind::Lhu => 2,
        LoadKind::Lw | LoadKind::Lwu => 4,
        LoadKind::Ld => 8,
    };

    let raw = match raw_load(ctx.mmu, ctx.bus, satp, virtual_address, size) {
        Ok(v) => v,
        Err(f) => {
            take_exception(ctx, f.mcause(), f.tval());
            return Err(());
        }
    };

    Ok(match kind {
        LoadKind::Lb => raw as u8 as i8 as i64 as u64,
        LoadKind::Lbu => raw as u8 as u64,
        LoadKind::Lh => raw as u16 as i16 as i64 as u64,
        LoadKind::Lhu => raw as u16 as u64,
        LoadKind::Lw => raw as u32 as i32 as i64 as u64,
        LoadKind::Lwu => raw as u32 as u64,
        LoadKind::Ld => raw,
    })
}

fn do_store<B: SystemBus>(
    ctx: &mut ExecContext<B>,
    satp: u64,
    kind: StoreKind,
    virtual_address: u64,
    val: u64,
) -> Result<(), ()> {
    let size: u64 = match kind {
        StoreKind::Sb => 1,
        StoreKind::Sh => 2,
        StoreKind::Sw => 4,
        StoreKind::Sd => 8,
    };

    match raw_store(
        ctx.mmu,
        ctx.bus,
        ctx.blocks,
        satp,
        virtual_address,
        val,
        size,
    ) {
        Ok(()) => Ok(()),
        Err(f) => {
            take_exception(ctx, f.mcause(), f.tval());
            Err(())
        }
    }
}

pub fn raw_load<B: SystemBus>(
    mmu: &mut Mmu,
    bus: &mut B,
    satp: u64,
    virtual_address: u64,
    size: u64,
) -> Result<u64, MmuFault> {
    if (virtual_address & 0xFFF) + size > 0x1000 {
        let mut buf = [0u8; 8];

        for i in 0..size {
            let byte_physical_address =
                mmu.translate_load(virtual_address.wrapping_add(i), satp, bus)?;
            buf[i as usize] = bus.read_byte(byte_physical_address);
        }

        Ok(u64::from_le_bytes(buf))
    } else {
        let physical_address = mmu.translate_load(virtual_address, satp, bus)?;

        Ok(match size {
            1 => bus.read_byte(physical_address) as u64,
            2 => bus.read_halfword(physical_address) as u64,
            4 => bus.read_word(physical_address) as u64,
            _ => bus.read_doubleword(physical_address),
        })
    }
}

pub fn raw_store<B: SystemBus>(
    mmu: &mut Mmu,
    bus: &mut B,
    blocks: &mut BlockCache,
    satp: u64,
    virtual_address: u64,
    val: u64,
    size: u64,
) -> Result<(), MmuFault> {
    if (virtual_address & 0xFFF) + size > 0x1000 {
        let bytes = val.to_le_bytes();

        for i in 0..size {
            let byte_physical_address =
                mmu.translate_store(virtual_address.wrapping_add(i), satp, bus)?;
            blocks.notify_store(byte_physical_address);
            bus.write_byte(byte_physical_address, bytes[i as usize]);
        }
    } else {
        let physical_address = mmu.translate_store(virtual_address, satp, bus)?;
        blocks.notify_store(physical_address);

        match size {
            1 => bus.write_byte(physical_address, val as u8),
            2 => bus.write_halfword(physical_address, val as u16),
            4 => bus.write_word(physical_address, val as u32),
            _ => bus.write_doubleword(physical_address, val),
        }
    }
    Ok(())
}

#[inline(always)]
pub fn effective_satp(priv_mode: PrivMode, satp: u64) -> u64 {
    if priv_mode == PrivMode::M { 0 } else { satp }
}

#[cfg(test)]
mod tests {
    use crate::system_bus::FlatMemory;
    use crate::{Hart, StepResult};

    fn mem_with(words: &[(u64, u32)]) -> FlatMemory {
        let mut mem = FlatMemory::new(1024 * 1024);
        for &(addr, w) in words {
            mem.load_at(addr as usize, &w.to_le_bytes());
        }
        mem
    }

    #[test]
    fn block_loop_sum() {
        let mut mem = mem_with(&[
            (0x00, 0x00000093), // addi x1, x0, 0
            (0x04, 0x00500113), // addi x2, x0, 5
            (0x08, 0x002080b3), // add  x1, x1, x2
            (0x0c, 0xfff10113), // addi x2, x2, -1
            (0x10, 0xfe011ce3), // bne  x2, x0, -8
            (0x14, 0x00000073), // ecall
        ]);
        let mut hart = Hart::new(0);

        assert_eq!(hart.run(&mut mem, 17), StepResult::Ok);
        assert_eq!(hart.regs.read(1), 15);
        assert_eq!(hart.regs.pc, 0x14);
    }

    #[test]
    fn block_loads_stores_and_fallthrough() {
        let mut mem = mem_with(&[
            (0x00, 0x10000093), // addi x1, x0, 256
            (0x04, 0x0ab00113), // addi x2, x0, 0xab
            (0x08, 0x0020a023), // sw   x2, 0(x1)
            (0x0c, 0x0000a183), // lw   x3, 0(x1)
            (0x10, 0x00000073), // ecall
        ]);
        let mut hart = Hart::new(0);

        assert_eq!(hart.run(&mut mem, 4), StepResult::Ok);
        assert_eq!(hart.regs.read(3), 0xab);
        assert_eq!(hart.regs.pc, 0x10);
    }

    #[test]
    fn store_invalidates_cached_block() {
        let mut mem = mem_with(&[
            (0x00, 0x04000113), // addi x2, x0, 0x40
            (0x04, 0x08002183), // lw   x3, 0x80(x0)
            (0x08, 0x00312023), // sw   x3, 0(x2)
            (0x0c, 0x0340006f), // jal  x0, +0x34
            (0x40, 0x00100093), // addi x1, x0, 1
            (0x44, 0x00000073), // ecall
            (0x80, 0x06300093), // addi x1, x0, 99
        ]);

        let mut hart = Hart::new(0x40);
        assert_eq!(hart.run(&mut mem, 1), StepResult::Ok);
        assert_eq!(hart.regs.read(1), 1);

        hart.regs.pc = 0;
        assert_eq!(hart.run(&mut mem, 4), StepResult::Ok);
        assert_eq!(hart.regs.pc, 0x40);

        assert_eq!(hart.run(&mut mem, 1), StepResult::Ok);
        assert_eq!(hart.regs.read(1), 99);
    }

    #[test]
    fn fence_i_flushes_blocks() {
        let mut mem = mem_with(&[
            (0x40, 0x00100093), // addi x1, x0, 1
            (0x44, 0x00000073), // ecall
        ]);
        let mut hart = Hart::new(0x40);
        assert_eq!(hart.run(&mut mem, 1), StepResult::Ok);
        assert_eq!(hart.regs.read(1), 1);

        mem.load_at(0x40, &0x06300093u32.to_le_bytes()); // addi x1, x0, 99
        mem.load_at(0x00, &0x0000100fu32.to_le_bytes()); // fence.i
        mem.load_at(0x04, &0x00000073u32.to_le_bytes()); // ecall

        hart.regs.pc = 0;
        assert_eq!(hart.run(&mut mem, 1), StepResult::Ok);

        hart.regs.pc = 0x40;
        assert_eq!(hart.run(&mut mem, 1), StepResult::Ok);
        assert_eq!(hart.regs.read(1), 99);
    }

    #[test]
    fn compressed_ops_in_blocks() {
        let mut mem = FlatMemory::new(1024 * 1024);

        mem.load_at(0x00, &0x4095u16.to_le_bytes()); // c.li x1, 5
        mem.load_at(0x02, &0x0089u16.to_le_bytes()); // c.addi x1, 2
        mem.load_at(0x04, &0x00000073u32.to_le_bytes()); // ecall
        let mut hart = Hart::new(0);

        assert_eq!(hart.run(&mut mem, 2), StepResult::Ok);
        assert_eq!(hart.regs.read(1), 7);
        assert_eq!(hart.regs.pc, 0x04);
    }
}
