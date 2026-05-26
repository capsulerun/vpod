// Reads the binary instruction and determines what actions the processor should take.

pub struct Instruction(pub u32);

impl Instruction {
    #[inline(always)]
    pub fn opcode(&self) -> u32 {
        self.0 & 0x7F
    }

    #[inline(always)]
    pub fn rd(&self) -> usize {
        ((self.0 >> 7) & 0x1F) as usize
    }

    #[inline(always)]
    pub fn rs1(&self) -> usize {
        ((self.0 >> 15) & 0x1F) as usize
    }

    #[inline(always)]
    pub fn rs2(&self) -> usize {
        ((self.0 >> 20) & 0x1F) as usize
    }

    #[inline(always)]
    pub fn funct3(&self) -> u32 {
        (self.0 >> 12) & 0x7
    }

    #[inline(always)]
    pub fn funct7(&self) -> u32 {
        self.0 >> 25
    }

    // I-type immediate
    #[inline(always)]
    pub fn imm_i(&self) -> i64 {
        ((self.0 as i32) >> 20) as i64
    }

    // S-type immediate
    #[inline(always)]
    pub fn imm_s(&self) -> i64 {
        let hi = (self.0 as i32) >> 25;
        let lo = ((self.0 >> 7) & 0x1F) as i32;
        ((hi << 5) | lo) as i64
    }

    // B-type immediate
    #[inline(always)]
    pub fn imm_b(&self) -> i64 {
        let raw = self.0;
        let bit12 = (raw >> 31) & 1;
        let bit11 = (raw >> 7) & 1;
        let bits10_5 = (raw >> 25) & 0x3F;
        let bits4_1 = (raw >> 8) & 0xF;
        let val = (bit12 << 12) | (bit11 << 11) | (bits10_5 << 5) | (bits4_1 << 1);

        sign_extend(val as i64, 13)
    }

    // U-type immediate
    #[inline(always)]
    pub fn imm_u(&self) -> i64 {
        ((self.0 & 0xFFFFF000) as i32) as i64
    }

    // J-type immediate
    #[inline(always)]
    pub fn imm_j(&self) -> i64 {
        let raw = self.0;
        let bit20 = (raw >> 31) & 1;
        let bits10_1 = (raw >> 21) & 0x3FF;
        let bit11 = (raw >> 20) & 1;
        let bits19_12 = (raw >> 12) & 0xFF;
        let val = (bit20 << 20) | (bits19_12 << 12) | (bit11 << 11) | (bits10_1 << 1);

        sign_extend(val as i64, 21)
    }

    // CSR address
    #[inline(always)]
    pub fn csr_addr(&self) -> u32 {
        self.0 >> 20
    }

    // For AMO
    #[inline(always)]
    pub fn funct5(&self) -> u32 {
        self.0 >> 27
    }

    // AMO acquire
    #[inline(always)]
    pub fn aq(&self) -> bool {
        (self.0 >> 26) & 1 != 0
    }

    // AMO release
    #[inline(always)]
    pub fn rl(&self) -> bool {
        (self.0 >> 25) & 1 != 0
    }
}

// 16-bit compressed instruction wrapper
pub struct CompressedInstruction(pub u16);

impl CompressedInstruction {
    #[inline(always)]
    pub fn quadrant(&self) -> u16 {
        self.0 & 0x3
    }

    #[inline(always)]
    pub fn funct3(&self) -> u16 {
        (self.0 >> 13) & 0x7
    }

    // CL/CS/CA format
    #[inline(always)]
    pub fn rs2_prime(&self) -> usize {
        ((self.0 >> 2) & 0x7) as usize + 8
    }

    // CL/CS/CA format
    #[inline(always)]
    pub fn rs1_prime(&self) -> usize {
        ((self.0 >> 7) & 0x7) as usize + 8
    }

    // Full rd/rs1 field for CR/CI formats
    #[inline(always)]
    pub fn rd(&self) -> usize {
        ((self.0 >> 7) & 0x1F) as usize
    }

    // Full rs2 field for CR/CSS formats
    #[inline(always)]
    pub fn rs2(&self) -> usize {
        ((self.0 >> 2) & 0x1F) as usize
    }
}

#[inline(always)]
pub fn sign_extend(val: i64, bits: u32) -> i64 {
    let shift = 64 - bits;
    (val << shift) >> shift
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_imm_s_negative() {
        let inst = Instruction(0xfa042823);
        assert_eq!(inst.imm_s(), -80);
    }

    #[test]
    fn test_imm_s_positive() {
        let inst = Instruction(0x00a12423);
        assert_eq!(inst.imm_s(), 8);
    }

    #[test]
    fn test_imm_i() {
        let inst = Instruction(0xfff00513);
        assert_eq!(inst.imm_i(), -1);
    }

    #[test]
    fn test_imm_b() {
        let inst = Instruction(0x00000063);
        assert_eq!(inst.imm_b(), 0);
    }

    #[test]
    fn test_imm_j() {
        let inst = Instruction(0x0000006f);
        assert_eq!(inst.imm_j(), 0);
    }

    #[test]
    fn test_imm_u() {
        let inst = Instruction(0x12345537);
        assert_eq!(inst.imm_u(), 0x12345000);
    }
}
