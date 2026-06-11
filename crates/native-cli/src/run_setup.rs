use std::path::PathBuf;

use machine::machine_bus::MachineBus;
use machine::snapshot::FLAG_SHELL_READY;
use riscv_core::{Hart, StepResult};

pub fn run(
    bus: &mut MachineBus,
    hart: &mut Hart,
    cmds: &[String],
    snap_save: Option<&PathBuf>,
    snap_flags: u8,
) {
    eprintln!("[vpod] setup booting guest, waiting for shell prompt...");
    bus.uart.capture_tx.set(true);

    let mut output = String::new();
    let mut cmd_idx = 0;
    let mut sent = false;

    const POLL_INTERVAL: u64 = 32768;

    loop {
        let interval = if bus.net_rx_pending() {
            4096
        } else {
            POLL_INTERVAL
        };

        bus.clint.advance_by_instructions(interval);
        bus.poll(hart);

        match hart.run(bus, interval) {
            StepResult::Ok => {}
            StepResult::Trap(cause) => {
                eprintln!("[vpod-setup] trap {:?} at pc={:#x}", cause, hart.regs.pc);
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
        let has_prompt = tail.contains("\n~ # ")
            || tail.contains("\n/ # ")
            || tail.contains("\n# ")
            || tail.starts_with("~ # ")
            || tail.starts_with("/ # ")
            || tail.starts_with("# ");

        if has_prompt {
            if !sent {
                for _ in 0..1000 {
                    bus.clint.advance_by_instructions(POLL_INTERVAL);
                    bus.poll(hart);
                    let _ = hart.run(bus, POLL_INTERVAL);
                }

                bus.uart.drain_rx();
                let _ = bus.uart.drain_tx();

                if cmd_idx < cmds.len() {
                    let cmd = &cmds[cmd_idx];
                    eprintln!("\n[vpod-setup] running: {}", cmd);
                    for b in cmd.bytes() {
                        bus.uart.push_rx(b);
                    }
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
                    eprintln!("\n[vpod-setup] running: {}", cmd);
                    bus.uart.drain_rx();
                    for b in cmd.bytes() {
                        bus.uart.push_rx(b);
                    }
                    bus.uart.push_rx(b'\n');
                } else {
                    break;
                }
            }
        }
    }

    if snap_flags & FLAG_SHELL_READY != 0 {
        eprintln!("[vpod-setup] running shell_init for warm snapshot...");
        shell_init(bus, hart);
    }

    if let Some(path) = snap_save {
        super::save_snapshot(bus, hart, path, snap_flags);
    }
    eprintln!("[vpod-setup] done.");
}

const PROMPT: &[u8] = b"# ";

fn shell_init(bus: &mut MachineBus, hart: &mut Hart) {
    for byte in b"stty -echo\n" {
        bus.uart.push_rx(*byte);
    }
    wait_for_prompt(bus, hart);
    bus.uart.drain_tx();

    let init_cmd = format!(
        "__ec() {{ printf \"\\x$(printf %02x $1)\" >/dev/ttyS2; }}; export PS1='$(__ec $?){}'; trap '__ec $?' EXIT\n",
        String::from_utf8_lossy(PROMPT)
    );
    for byte in init_cmd.bytes() {
        bus.uart.push_rx(byte);
    }
    wait_for_prompt(bus, hart);
    bus.uart.drain_tx();
    bus.uart_stderr.drain_tx();
    bus.uart_ctrl.drain_tx();
}

fn wait_for_prompt(bus: &mut MachineBus, hart: &mut Hart) {
    let mut buffer = Vec::new();
    for _ in 0..500_000u32 {
        if hart.is_waiting {
            hart.is_waiting = false;
        }
        bus.clint.advance_by_instructions(8192);
        bus.poll(hart);
        match hart.run(bus, 8192) {
            StepResult::Ok => {}
            StepResult::Trap(_) | StepResult::Halt => return,
        }
        let output = bus.uart.drain_tx();
        if !output.is_empty() {
            buffer.extend_from_slice(&output);
            if buffer.ends_with(PROMPT) {
                return;
            }
        }
    }
}
