pub mod block;
pub mod csr;
pub mod decode;
pub mod execute;
pub mod extensions;
pub mod gpr;
pub mod hart;
pub mod mmu;
pub mod perf;
pub mod system_bus;
pub mod trap;

pub use csr::{Csr, PrivMode};
pub use hart::Hart;
pub use mmu::Mmu;
pub use system_bus::{FlatMemory, SystemBus};
pub use trap::{StepResult, TrapCause};

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cpu_mem(program: &[u32]) -> (Hart, FlatMemory) {
        let mut mem = FlatMemory::new(1024 * 1024); // just 1 MB
        let bytes: Vec<u8> = program.iter().flat_map(|w| w.to_le_bytes()).collect();

        mem.load_at(0, &bytes);
        (Hart::new(0), mem)
    }

    fn run(program: &[u32]) -> (Hart, FlatMemory) {
        let (mut cpu, mut mem) = make_cpu_mem(program);

        let steps = program.len() - 1;
        for _ in 0..steps {
            cpu.step(&mut mem);
        }

        (cpu, mem)
    }

    #[test]
    fn addi() {
        let (cpu, _) = run(&[0x02a00093, 0x00000073]); // ADDI x1, x0, 42   (x1 = 42) + ECALL
        assert_eq!(cpu.regs.read(1), 42);
    }

    #[test]
    fn addi_negative() {
        let (cpu, _) = run(&[0xfff00093, 0x00000073]); // ADDI x1, x0, -1 + ECALL
        assert_eq!(cpu.regs.read(1) as i64, -1);
    }

    #[test]
    fn lui() {
        let (cpu, _) = run(&[0x000010b7, 0x00000073]); // LUI x1, 1   (x1 = 0x1000) + ECALL
        assert_eq!(cpu.regs.read(1), 0x1000);
    }

    #[test]
    fn auipc() {
        let (cpu, _) = run(&[0x00000097, 0x00000073]); // AUIPC x1, 0  (x1 = pc = 0) + ECALL
        assert_eq!(cpu.regs.read(1), 0);
    }

    #[test]
    fn add() {
        // ADDI x1, x0, 10
        // ADDI x2, x0, 20
        // ADD  x3, x1, x2   (x3 = 30)
        let (cpu, _) = run(&[0x00a00093, 0x01400113, 0x002081b3, 0x00000073]);
        assert_eq!(cpu.regs.read(3), 30);
    }

    #[test]
    fn sub() {
        // ADDI x1, x0, 10
        // ADDI x2, x0, 3
        // SUB  x3, x1, x2   (x3 = 7)
        let (cpu, _) = run(&[0x00a00093, 0x00300113, 0x402081b3, 0x00000073]);
        assert_eq!(cpu.regs.read(3), 7);
    }

    #[test]
    fn xori() {
        // ADDI x1, x0, 0xff
        // XORI x2, x1, 0x0f   (x2 = 0xf0)
        let (cpu, _) = run(&[0x0ff00093, 0x00f0c113, 0x00000073]);
        assert_eq!(cpu.regs.read(2), 0xf0);
    }

    #[test]
    fn slli() {
        // ADDI x1, x0, 1
        // SLLI x2, x1, 4    (x2 = 16)
        let (cpu, _) = run(&[0x00100093, 0x00409113, 0x00000073]);
        assert_eq!(cpu.regs.read(2), 16);
    }

    #[test]
    fn srli() {
        // ADDI x1, x0, 16
        // SRLI x2, x1, 2    (x2 = 4)
        let (cpu, _) = run(&[0x01000093, 0x0020d113, 0x00000073]);
        assert_eq!(cpu.regs.read(2), 4);
    }

    #[test]
    fn srai() {
        // ADDI x1, x0, -8    (x1 = 0xffff_ffff_ffff_fff8)
        // SRAI x2, x1, 1     (x2 = -4 sign-extended)
        let (cpu, _) = run(&[0xff800093, 0x4010d113, 0x00000073]);
        assert_eq!(cpu.regs.read(2) as i64, -4);
    }

    #[test]
    fn beq_taken() {
        let (cpu, _) = run(&[
            0x00500093, // ADDI x1, x0, 5
            0x00500113, // ADDI x2, x0, 5
            0x00208463, // BEQ  x1, x2, +8
            0x00100193, // ADDI x3, x0, 1  (skipped)
            0x06300193, // ADDI x3, x0, 99
            0x00000073, // ECALL
        ]);
        assert_eq!(cpu.regs.read(3), 99);
    }

    #[test]
    fn bne_not_taken() {
        let (cpu, _) = run(&[
            0x00700093, // ADDI x1, x0, 7
            0x00700113, // ADDI x2, x0, 7
            0x00209463, // BNE  x1, x2, +8
            0x02a00193, // ADDI x3, x0, 42
            0x00000073, // ECALL
        ]);

        assert_eq!(cpu.regs.read(3), 42);
    }

    #[test]
    fn sw_lw() {
        let (cpu, _) = run(&[
            0x10000093, // ADDI x1, x0, 256
            0x0ab00113, // ADDI x2, x0, 0xab
            0x0020a023, // SW   x2, 0(x1)
            0x0000a183, // LW   x3, 0(x1)
            0x00000073, // ECALL
        ]);

        assert_eq!(cpu.regs.read(3), 0xab);
    }

    #[test]
    fn sb_lb_sign_extend() {
        let (cpu, _) = run(&[
            0x20000093, // ADDI x1, x0, 512
            0xfff00113, // ADDI x2, x0, -1
            0x00208023, // SB   x2, 0(x1)
            0x00008183, // LB   x3, 0(x1)
            0x00000073, // ECALL
        ]);

        assert_eq!(cpu.regs.read(3) as i64, -1);
    }

    #[test]
    fn jal() {
        let (cpu, _) = run(&[
            0x008000ef, // JAL x1, +8
            0x00100113, // ADDI x2, x0, 1  (skipped)
            0x00200113, // ADDI x2, x0, 2
            0x00000073, // ECALL
        ]);

        assert_eq!(cpu.regs.read(2), 2);
        assert_eq!(cpu.regs.read(1), 4); // return address = 0 + 4
    }

    #[test]
    fn addiw() {
        let (cpu, _) = run(&[
            0xfff00093, // ADDI  x1, x0, -1
            0x0010809b, // ADDIW x2, x1,  1
            0x00000073,
        ]);

        assert_eq!(cpu.regs.read(2), 0);
    }

    #[test]
    fn write_to_x0_is_ignored() {
        let (cpu, _) = run(&[0x06300013, 0x00000073]); // ADDI x0, x0, 99
        assert_eq!(cpu.regs.read(0), 0);
    }

    #[test]
    fn mul() {
        // ADDI x1, x0, 6
        // ADDI x2, x0, 7
        // MUL  x3, x1, x2   (x3 = 42)
        let (cpu, _) = run(&[0x00600093, 0x00700113, 0x022081b3, 0x00000073]);
        assert_eq!(cpu.regs.read(3), 42);
    }

    #[test]
    fn div_exact() {
        // ADDI x1, x0, 42
        // ADDI x2, x0, 6
        // DIV  x3, x1, x2   (x3 = 7)
        let (_cpu, _mem) = run(&[0x02a00093, 0x00600113, 0x022081b3, 0x00000073]);
        assert_eq!(crate::extensions::div(42, 6), 7);
    }

    #[test]
    fn div_by_zero() {
        assert_eq!(crate::extensions::divu(100, 0), u64::MAX);
        assert_eq!(crate::extensions::div(100, 0), u64::MAX);
    }

    #[test]
    fn rem_signed() {
        assert_eq!(crate::extensions::rem((-7i64) as u64, 3), (-1i64) as u64);
    }

    #[test]
    fn mulh_signed() {
        assert_eq!(crate::extensions::mulh(1u64 << 62, 4), 1);
    }

    #[test]
    fn csrrw_mscratch() {
        let (cpu, _) = run(&[0x05500093, 0x34009173, 0x00000073]);

        assert_eq!(cpu.regs.read(2), 0);
        assert_eq!(cpu.csr.mscratch, 0x55);
    }

    #[test]
    fn csrrs_misa_readonly() {
        let (cpu, _) = run(&[0x301010f3, 0x00000073]);

        assert!(cpu.regs.read(1) & (1 << 8) != 0); // I bit set
        assert!(cpu.regs.read(1) & (1 << 12) != 0); // M bit set
    }
}
