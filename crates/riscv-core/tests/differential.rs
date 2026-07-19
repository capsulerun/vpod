// Differential harness: interpreter vs AOT lockstep.

#![cfg(feature = "aot")]

use riscv_core::Hart;
use riscv_core::system_bus::FlatMemory;

fn assert_state_eq(a: &Hart, b: &Hart, entry: u64, step: u32) {
    let ctx = |what: &str| {
        format!(
            "{what} diverged: program 0x{entry:x}, block step {step}, interp pc=0x{:x} aot pc=0x{:x}",
            a.regs.pc, b.regs.pc
        )
    };

    assert_eq!(a.regs.pc, b.regs.pc, "{}", ctx("pc"));
    for i in 0..32 {
        assert_eq!(a.regs.read(i), b.regs.read(i), "{}", ctx(&format!("x{i}")));
    }
    assert_eq!(a.priv_mode, b.priv_mode, "{}", ctx("priv_mode"));
    assert_eq!(a.csr.instret, b.csr.instret, "{}", ctx("instret"));
    assert_eq!(a.csr.mcause, b.csr.mcause, "{}", ctx("mcause"));
    assert_eq!(a.csr.mepc, b.csr.mepc, "{}", ctx("mepc"));
    assert_eq!(a.csr.mstatus, b.csr.mstatus, "{}", ctx("mstatus"));
    assert_eq!(a.csr.mtval, b.csr.mtval, "{}", ctx("mtval"));
    assert_eq!(a.is_waiting, b.is_waiting, "{}", ctx("is_waiting"));
}

#[test]
fn aot_differential_lockstep() {
    let Ok(dir) = std::env::var("VPOD_DIFF_DIR") else {
        eprintln!("VPOD_DIFF_DIR not set — run via scripts/aot-diff.sh; skipping");
        return;
    };

    let ram = std::fs::read(format!("{dir}/ram.bin")).expect("ram.bin");
    let entries: Vec<u64> = std::fs::read_to_string(format!("{dir}/entries.txt"))
        .expect("entries.txt")
        .lines()
        .filter_map(|l| u64::from_str_radix(l.trim(), 16).ok())
        .collect();
    let trap_pa = ram.len() as u64 - 4096;

    assert!(!entries.is_empty(), "no program entries");
    assert!(
        !riscv_core::aot::AOT_PAGE_HASHES.is_empty(),
        "generated.rs is the stub — regenerate via scripts/aot-diff.sh"
    );

    let mut terminated = 0usize;
    for &entry in &entries {
        let mut mem_interp = FlatMemory::new(ram.len());
        mem_interp.load_at(0, &ram);
        let mut mem_aot = FlatMemory::new(ram.len());
        mem_aot.load_at(0, &ram);

        let mut interp = Hart::new(entry);
        let mut aot = Hart::new(entry);
        interp.csr.mtvec = trap_pa;
        aot.csr.mtvec = trap_pa;
        aot.blocks.aot_init(riscv_core::aot::AOT_PAGE_HASHES);

        for step in 0..20_000u32 {
            interp.run(&mut mem_interp, 1);
            aot.run(&mut mem_aot, 1);
            assert_state_eq(&interp, &aot, entry, step);

            if interp.is_waiting {
                break;
            }
        }

        if interp.is_waiting {
            terminated += 1;
        }

        for &chunk in &[7u64, 97, 1024] {
            let mut mem_interp = FlatMemory::new(ram.len());
            mem_interp.load_at(0, &ram);
            let mut mem_aot = FlatMemory::new(ram.len());
            mem_aot.load_at(0, &ram);

            let mut interp = Hart::new(entry);
            let mut aot = Hart::new(entry);
            interp.csr.mtvec = trap_pa;
            aot.csr.mtvec = trap_pa;
            aot.blocks.aot_init(riscv_core::aot::AOT_PAGE_HASHES);

            for step in 0..(40_000 / chunk as u32).max(64) {
                interp.run(&mut mem_interp, chunk);
                aot.run(&mut mem_aot, chunk);
                assert_state_eq(&interp, &aot, entry, step);

                if interp.is_waiting {
                    break;
                }
            }
        }

        {
            let reloc_len = ram.len() * 2;
            let reloc_entry = ram.len() as u64 + (entry & !0xfff);

            let mut mem_interp = FlatMemory::new(reloc_len);
            mem_interp.load_at(0, &ram);
            let code_page: Vec<u8> =
                ram[(entry & !0xfff) as usize..((entry & !0xfff) + 4096) as usize].to_vec();
            mem_interp.load_at(reloc_entry as usize, &code_page);
            let mut mem_aot = FlatMemory::new(reloc_len);
            mem_aot.load_at(0, &ram);
            mem_aot.load_at(reloc_entry as usize, &code_page);

            let mut interp = Hart::new(reloc_entry);
            let mut aot = Hart::new(reloc_entry);
            interp.csr.mtvec = trap_pa;
            aot.csr.mtvec = trap_pa;
            aot.blocks.aot_init(riscv_core::aot::AOT_PAGE_HASHES);

            for step in 0..2_000u32 {
                interp.run(&mut mem_interp, 512);
                aot.run(&mut mem_aot, 512);
                assert_state_eq(&interp, &aot, entry, step);

                if interp.is_waiting {
                    break;
                }
            }
        }
    }

    let dispatched = riscv_core::aot::DISPATCH_RETIRED.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        dispatched > 0,
        "AOT dispatch never fired — the comparison was interpreter vs interpreter"
    );

    eprintln!(
        "[diff] {} programs ({terminated} reached the trap vector), {dispatched} insns retired via aot, lockstep state identical throughout",
        entries.len()
    );
}
