// Recommend to directly use the wasm component instead.
// This is mostly made for debug purposes.

use std::path::PathBuf;

use machine::machine_bus::MachineBus;
use riscv_core::{Hart, StepResult};

use crate::terminal;

pub fn run(
    bus: &mut MachineBus,
    hart: &mut Hart,
    snap_save: Option<&PathBuf>,
    trace_insns: u64,
    snap_flags: u8,
) {
    if snap_save.is_some() {
        eprintln!("[vpod] Press Ctrl-S to save snapshot. Press Ctrl-C to exit.");
    } else {
        eprintln!("[vpod] Press Ctrl-C to exit the emulator.");
    }

    if trace_insns > 0 {
        run_trace(bus, hart, trace_insns);
        return;
    }

    let _raw = terminal::RawTerminal::enter();
    terminal::set_nonblocking();

    const POLL_INTERVAL: u64 = 32768;
    const POLL_INTERVAL_NET: u64 = 4096;
    let mut steps: u64 = 0;
    let sample_at: Vec<u64> = (1..=10).map(|i| i * 5_000_000).collect();
    let mut sample_idx = 0;

    loop {
        let interval = if bus.net_rx_pending() {
            POLL_INTERVAL_NET
        } else {
            POLL_INTERVAL
        };
        bus.clint.advance_by_instructions(interval);
        bus.poll(hart);
        terminal::poll_stdin(bus, snap_save, hart, snap_flags);

        bus.flush_console_to_stdout();
        match hart.run(bus, interval) {
            StepResult::Ok => {}
            StepResult::Trap(cause) => {
                eprintln!(
                    "\r\n[vpod] unhandled trap {:?} at pc={:#x}",
                    cause, hart.regs.pc
                );
                break;
            }
            StepResult::Halt => {
                eprintln!("\r\n[vpod] halt after {} steps", steps);
                break;
            }
        }
        steps += interval;

        if sample_idx < sample_at.len() && steps >= sample_at[sample_idx] {
            eprintln!(
                "[vpod] @ {}M insns: pc={:#x}  mtvec={:#x}  mcause={:#x}  mepc={:#x}",
                steps / 1_000_000,
                hart.regs.pc,
                hart.csr.mtvec,
                hart.csr.mcause,
                hart.csr.mepc
            );
            sample_idx += 1;
        }
    }
}

fn run_trace(bus: &mut MachineBus, hart: &mut Hart, trace_insns: u64) {
    struct Ms {
        addr: u64,
        name: &'static str,
        dump_regs: bool,
    }
    let milestones: &[Ms] = &[
        Ms {
            addr: 0x80017030,
            name: "fw_platform_init entry (a0=hart_id a1=fdt_pa)",
            dump_regs: true,
        },
        Ms {
            addr: 0x80019410,
            name: "fdt_parse_hart_id called (a0=fdt a1=cpu_node)",
            dump_regs: true,
        },
        Ms {
            addr: 0x80019472,
            name: "fdt_parse_hart_id ret (a0=0→ok/-3→fail)",
            dump_regs: true,
        },
        Ms {
            addr: 0x80019392,
            name: "fdt_node_is_enabled called (a0=fdt a1=node)",
            dump_regs: true,
        },
        Ms {
            addr: 0x8001940e,
            name: "fdt_node_is_enabled ret (a0=0→disabled/1→enabled)",
            dump_regs: true,
        },
        Ms {
            addr: 0x80017160,
            name: "fw_platform_init: after CPU loop (s6=hart_count)",
            dump_regs: true,
        },
        Ms {
            addr: 0x8000ab82,
            name: "sbi_init",
            dump_regs: false,
        },
        Ms {
            addr: 0x8000ac38,
            name: "init_coldboot",
            dump_regs: false,
        },
        Ms {
            addr: 0x80010b18,
            name: "sbi_scratch_init",
            dump_regs: false,
        },
        Ms {
            addr: 0x8000a100,
            name: "sbi_heap_init",
            dump_regs: false,
        },
        Ms {
            addr: 0x800095b8,
            name: "sbi_domain_init",
            dump_regs: false,
        },
        Ms {
            addr: 0x8000a438,
            name: "sbi_hsm_init",
            dump_regs: false,
        },
        Ms {
            addr: 0x800023f6,
            name: "sbi_hart_init entry",
            dump_regs: false,
        },
        Ms {
            addr: 0x800024b6,
            name: "hart_init: call platform_extensions_init",
            dump_regs: false,
        },
        Ms {
            addr: 0x80017480,
            name: "generic_extensions_init",
            dump_regs: false,
        },
        Ms {
            addr: 0x80019634,
            name: "fdt_parse_isa_extensions entry (a0=fdt a1=hartid)",
            dump_regs: true,
        },
        Ms {
            addr: 0x80010ade,
            name: "sbi_hartid_to_hartindex called (a0=hartid)",
            dump_regs: true,
        },
        Ms {
            addr: 0x800024b8,
            name: "hart_init: returned from platform_extensions_init (a0=rc)",
            dump_regs: true,
        },
        Ms {
            addr: 0x800032f2,
            name: "hart_init: error return ← FAIL",
            dump_regs: true,
        },
        Ms {
            addr: 0x8000251a,
            name: "hart_init: csrrw mtvec (CSR probe — success path)",
            dump_regs: false,
        },
        Ms {
            addr: 0x80004b1c,
            name: "sbi_hart_hang ← FAIL",
            dump_regs: true,
        },
    ];
    let mut visited = vec![false; milestones.len()];

    const BATCH: u64 = 8192;
    let mut total: u64 = 0;
    'outer: loop {
        bus.clint.advance_by_instructions(BATCH);
        bus.poll(hart);

        for _ in 0..BATCH {
            let pc = hart.regs.pc;
            for (idx, ms) in milestones.iter().enumerate() {
                if !visited[idx] && pc == ms.addr {
                    if ms.dump_regs {
                        eprintln!(
                            "[milestone @{}] pc={:#x}  {}  a0={:#x} a1={:#x} a2={:#x} a3={:#x}",
                            total,
                            pc,
                            ms.name,
                            hart.regs.read(10),
                            hart.regs.read(11),
                            hart.regs.read(12),
                            hart.regs.read(13)
                        );
                    } else {
                        eprintln!("[milestone @{}] pc={:#x}  {}", total, pc, ms.name);
                    }

                    visited[idx] = true;

                    if ms.name.contains("FAIL") {
                        break 'outer;
                    }
                }
            }

            match hart.step(bus) {
                riscv_core::StepResult::Ok => {}
                riscv_core::StepResult::Trap(c) => {
                    eprintln!("[milestone] trap {:?} @ {:#x}", c, hart.regs.pc);
                    break 'outer;
                }
                riscv_core::StepResult::Halt => {
                    eprintln!("[milestone] halt");
                    break 'outer;
                }
            }
            total += 1;
            if total >= trace_insns {
                break 'outer;
            }
        }
    }
    eprintln!(
        "[milestone] stopped at {} insns, pc={:#x}",
        total, hart.regs.pc
    );
}
