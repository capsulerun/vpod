use std::path::PathBuf;

use machine::machine_bus::MachineBus;
use riscv_core::{Hart, StepResult};

pub fn run(bus: &mut MachineBus, hart: &mut Hart, cmds: &[String], snap_save: Option<&PathBuf>) {
    eprintln!("[capsule] setup booting guest, waiting for shell prompt...");
    bus.uart.capture_tx.set(true);

    let mut output = String::new();
    let mut cmd_idx = 0;
    let mut sent = false;

    const POLL_INTERVAL: u64 = 32768;

    loop {
        let interval = if bus.net_rx_pending() { 4096 } else { POLL_INTERVAL };
        bus.clint.advance(interval);
        bus.poll(hart);
        match hart.run(bus, interval) {
            StepResult::Ok => {}
            StepResult::Trap(cause) => {
                eprintln!("[capsule-setup] trap {:?} at pc={:#x}", cause, hart.regs.pc);
                std::process::exit(1);
            }
            StepResult::Halt => break,
        }
        let bytes = bus.uart.drain_tx();
        if !bytes.is_empty() {
            let text = String::from_utf8_lossy(&bytes);
            eprint!("{}", text);
            output.push_str(&text);
        }

        let tail = &output[output.len().saturating_sub(128)..];
        let has_prompt = tail.contains("\n~ # ") || tail.contains("\n/ # ")
            || tail.starts_with("~ # ") || tail.starts_with("/ # ");

        if has_prompt {
            if !sent {
                for _ in 0..1000 {
                    bus.clint.advance(POLL_INTERVAL);
                    bus.poll(hart);
                    let _ = hart.run(bus, POLL_INTERVAL);
                }
                bus.uart.drain_rx();
                let _ = bus.uart.drain_tx();

                if cmd_idx < cmds.len() {
                    let cmd = &cmds[cmd_idx];
                    eprintln!("\n[capsule-setup] running: {}", cmd);
                    for b in cmd.bytes() { bus.uart.push_rx(b); }
                    bus.uart.push_rx(b'\n');
                    sent = true;
                    output.clear();
                } else {
                    output.clear();
                    break;
                }
            } else {
                output.clear();
                cmd_idx += 1;
                if cmd_idx < cmds.len() {
                    let cmd = &cmds[cmd_idx];
                    eprintln!("\n[capsule-setup] running: {}", cmd);
                    bus.uart.drain_rx();
                    for b in cmd.bytes() { bus.uart.push_rx(b); }
                    bus.uart.push_rx(b'\n');
                } else {
                    break;
                }
            }
        }
    }

    if let Some(path) = snap_save {
        super::save_snapshot(bus, hart, path);
    }
    eprintln!("[capsule-setup] done.");
}
