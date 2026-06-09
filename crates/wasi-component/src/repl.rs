use machine::machine_bus::MachineBus;
use riscv_core::{Hart, StepResult};
use wasi::clocks::monotonic_clock;
use wasi::io::poll;

const STEP: u64 = 8192;

const NET_YIELD_NS: u64 = 5_000_000; // 5 ms

pub fn shell_init(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8]) {
    for byte in b"stty -echo\n" {
        bus.uart.push_rx(*byte);
    }
    wait_for_prompt(bus, hart, prompt);
    bus.uart.drain_tx();

    let init_cmd = format!(
        "__ec() {{ printf \"\\x$(printf %02x $1)\" >/dev/ttyS2; }}; export PS1='$(__ec $?){}'; trap '__ec $?' EXIT\n",
        String::from_utf8_lossy(prompt)
    );
    for byte in init_cmd.bytes() {
        bus.uart.push_rx(byte);
    }
    wait_for_prompt(bus, hart, prompt);
    bus.uart.drain_tx();
    bus.uart_stderr.drain_tx();
    bus.uart_ctrl.drain_tx();
}

pub fn wait_for_prompt(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8]) {
    let mut buffer = Vec::new();

    for _ in 0..500_000u32 {
        if hart.is_waiting {
            hart.is_waiting = false;
        }

        bus.clint.advance_by_instructions(STEP);
        bus.poll(hart);

        match hart.run(bus, STEP) {
            StepResult::Ok => {}
            StepResult::Trap(_) | StepResult::Halt => return,
        }

        let output = bus.uart.drain_tx();
        if !output.is_empty() {
            buffer.extend_from_slice(&output);
            if buffer.ends_with(prompt) {
                return;
            }
        }
    }
}

pub fn capture_output_until_prompt(
    bus: &mut MachineBus,
    hart: &mut Hart,
    prompt: &[u8],
    timeout_secs: u64,
) -> String {
    let deadline = monotonic_clock::now() + timeout_secs * 1_000_000_000;

    let mut output = Vec::new();
    let mut wfi_count = 0u32;
    let mut got_output = false;

    loop {
        if monotonic_clock::now() >= deadline {
            break;
        }

        if hart.is_waiting {
            hart.is_waiting = false;

            if !bus.has_pending_io() {
                let timeout = monotonic_clock::subscribe_duration(NET_YIELD_NS);
                poll::poll(&[&timeout]);


                if got_output && !bus.net_rx_pending() && !bus.net_has_active_connections() {
                    wfi_count += 1;
                    if wfi_count >= 32 {
                        break;
                    }
                }
            } else {
                wfi_count = 0;
            }
        } else {
            wfi_count = 0;
        }

        bus.clint.advance_by_instructions(STEP);
        bus.poll(hart);

        match hart.run(bus, STEP) {
            StepResult::Ok => {}
            StepResult::Trap(_) | StepResult::Halt => break,
        }

        let tx = bus.uart.drain_tx();
        if !tx.is_empty() {
            output.extend_from_slice(&tx);
            got_output = true;
            wfi_count = 0;

            if output.ends_with(prompt) {
                output.truncate(output.len() - prompt.len());
                break;
            }
        }
    }

    if !output.is_empty() && !output.ends_with(b"\n") {
        if let Some(pos) = output.iter().rposition(|&b| b == b'\n') {
            output.truncate(pos + 1);
        } else {
            output.clear();
        }
    }

    let raw = String::from_utf8_lossy(&output);
    strip_ansi(&raw).trim_end().to_string()
}

fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}
