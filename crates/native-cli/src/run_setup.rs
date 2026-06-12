use std::path::PathBuf;

use machine::machine_bus::MachineBus;
use machine::snapshot::FLAG_SHELL_READY;
use riscv_core::{Hart, StepResult};

const STEP: u64 = 32768;
const PROMPT: &[u8] = b"# ";

pub fn run(
    bus: &mut MachineBus,
    hart: &mut Hart,
    cmds: &[String],
    snap_save: Option<&PathBuf>,
    snap_flags: u8,
) {
    eprintln!("[vpod] setup: booting guest, waiting for shell prompt...");
    bus.uart.capture_tx.set(true);

    wait_for_prompt(bus, hart, true);

    for (i, cmd) in cmds.iter().enumerate() {
        eprintln!("[vpod-setup] ({}/{}): {}", i + 1, cmds.len(), cmd);
        drain_all(bus);
        push_line(bus, cmd.as_bytes());
        wait_for_prompt(bus, hart, true);
    }

    let mut final_flags = snap_flags;
    if snap_flags & FLAG_SHELL_READY != 0 {
        eprintln!("[vpod-setup] running shell_init for warm snapshot...");
        drain_all(bus);

        if !shell_init(bus, hart) {
            eprintln!("[vpod-setup] shell_init failed, saving cold snapshot");
            final_flags &= !FLAG_SHELL_READY;
        }
    }

    if let Some(path) = snap_save {
        super::save_snapshot(bus, hart, path, final_flags);
    }
    eprintln!("[vpod-setup] done.");
}

fn shell_init(bus: &mut MachineBus, hart: &mut Hart) -> bool {
    push_line(bus, b"stty -echo; rm -f ~/.ash_history; HISTFILE=/dev/null HISTSIZE=0 exec sh");
    wait_for_prompt(bus, hart, false);
    drain_all(bus);

    let init_cmd = format!(
        "__ec() {{ printf \"\\x$(printf %02x $1)\" >/dev/ttyS2; }}; export PS1='$(__ec $?){}'; trap '__ec $?' EXIT",
        String::from_utf8_lossy(PROMPT)
    );
    push_line(bus, init_cmd.as_bytes());
    wait_for_prompt(bus, hart, false);
    drain_all(bus);

    push_line(bus, b"echo VPOD_INIT_OK");
    let output = wait_for_prompt(bus, hart, false);
    let text = String::from_utf8_lossy(&output);

    if !text.contains("VPOD_INIT_OK") {
        eprintln!("[vpod-setup] verification failed: {:?}", text);
        false
    } else if text.contains("echo VPOD_INIT_OK") {
        eprintln!("[vpod-setup] stty -echo not active");
        false
    } else {
        eprintln!("[vpod-setup] shell_init verified OK");
        drain_all(bus);
        true
    }
}

fn wait_for_prompt(bus: &mut MachineBus, hart: &mut Hart, verbose: bool) -> Vec<u8> {
    let mut buffer = Vec::new();
    let mut dsr_answered = false;
    for _ in 0..2_000_000u32 {
        if hart.is_waiting {
            hart.is_waiting = false;
        }
        let interval = if bus.net_rx_pending() { 4096 } else { STEP };
        bus.clint.advance_by_instructions(interval);
        bus.poll(hart);
        match hart.run(bus, interval) {
            StepResult::Ok => {}
            StepResult::Trap(cause) => {
                eprintln!("[vpod-setup] trap {:?} at pc={:#x}", cause, hart.regs.pc);
                std::process::exit(1);
            }
            StepResult::Halt => return buffer,
        }
        let tx = bus.uart.drain_tx();
        if !tx.is_empty() {
            if verbose {
                eprint!("{}", String::from_utf8_lossy(&tx));
            }
            buffer.extend_from_slice(&tx);

            if !dsr_answered && buffer.windows(4).any(|w| w == b"\x1b[6n") {
                for &b in b"\x1b[1;1R" {
                    bus.uart.push_rx(b);
                }
                dsr_answered = true;
            }

            let tail = String::from_utf8_lossy(&buffer[buffer.len().saturating_sub(32)..]);
            if tail.contains("# ") {
                return buffer;
            }
        }
    }
    eprintln!(
        "[vpod-setup] wait_for_prompt timed out, tail: {:?}",
        String::from_utf8_lossy(&buffer[buffer.len().saturating_sub(80)..])
    );
    buffer
}

fn push_line(bus: &mut MachineBus, data: &[u8]) {
    for &b in data {
        bus.uart.push_rx(b);
    }
    bus.uart.push_rx(b'\n');
}

fn drain_all(bus: &mut MachineBus) {
    bus.uart.drain_tx();
    bus.uart.drain_rx();
    bus.uart_stderr.drain_tx();
    bus.uart_ctrl.drain_tx();
}
