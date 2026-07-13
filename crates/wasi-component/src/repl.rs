use machine::machine_bus::MachineBus;
use riscv_core::{Hart, StepResult};
use wasi::clocks::monotonic_clock;
use wasi::clocks::wall_clock;
use wasi::io::poll;

const STEP: u64 = 8192;

const NET_YIELD_NS: u64 = 5_000_000; // 5 ms

// TO TEST : the time UART must be quiet after last output before declare the command
const QUIET_PERIOD_NS: u64 = 150_000_000; // 150 ms

pub fn sync_clock(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8]) {
    let now = wall_clock::now();
    let date_cmd = format!("date -s @{}\n", now.seconds);

    for byte in date_cmd.bytes() {
        bus.uart.push_rx(byte);
    }

    wait_for_prompt(bus, hart, prompt);

    bus.uart.drain_tx();
    bus.uart_stderr.drain_tx();
    bus.uart_ctrl.drain_tx();
}

pub fn shell_init(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8]) {
    sync_clock(bus, hart, prompt);

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

pub fn capture_output(
    bus: &mut MachineBus,
    hart: &mut Hart,
    prompt: &[u8],
    timeout_secs: u64,
    stop_on_ctrl: bool,
    sentinel: Option<&str>,
    data_channel: bool,
) -> String {
    let deadline = monotonic_clock::now() + timeout_secs * 1_000_000_000;

    let mut output = Vec::new();
    let mut last_output_ns = monotonic_clock::now();
    let mut got_output = false;
    let mut ended_at_prompt = false;

    loop {
        let now = monotonic_clock::now();
        if now >= deadline {
            break;
        }

        if hart.is_waiting {
            hart.is_waiting = false;

            if !bus.has_pending_io() {
                let timeout = monotonic_clock::subscribe_duration(NET_YIELD_NS);
                poll::poll(&[&timeout]);

                if !data_channel
                    && sentinel.is_none()
                    && got_output
                    && !bus.net_rx_pending()
                    && !bus.net_has_active_connections()
                    && monotonic_clock::now().saturating_sub(last_output_ns) >= QUIET_PERIOD_NS
                {
                    break;
                }
            }
        }

        bus.clint.advance_by_instructions(STEP);
        bus.poll(hart);

        match hart.run(bus, STEP) {
            StepResult::Ok => {}
            StepResult::Trap(_) | StepResult::Halt => {
                if stop_on_ctrl && !bus.uart_ctrl.tx_is_empty() {
                    bus.uart.drain_tx();
                }
                break;
            }
        }

        let tx = if data_channel {
            bus.uart_data.drain_tx()
        } else {
            bus.uart.drain_tx()
        };

        if !tx.is_empty() {
            output.extend_from_slice(&tx);
            got_output = true;
            last_output_ns = monotonic_clock::now();

            if !data_channel && output.ends_with(prompt) {
                output.truncate(output.len() - prompt.len());
                ended_at_prompt = true;
                break;
            }

            if let Some(s) = sentinel
                && let Ok(text) = std::str::from_utf8(&output)
                && text.contains(s)
            {
                break;
            }
        }

        if !data_channel && stop_on_ctrl && !bus.uart_ctrl.tx_is_empty() {
            for _ in 0..64 {
                bus.clint.advance_by_instructions(STEP);
                bus.poll(hart);

                match hart.run(bus, STEP) {
                    StepResult::Ok => {}
                    StepResult::Trap(_) | StepResult::Halt => break,
                }

                let extra = bus.uart.drain_tx();
                if !extra.is_empty() {
                    output.extend_from_slice(&extra);

                    if output.ends_with(prompt) {
                        output.truncate(output.len() - prompt.len());
                        ended_at_prompt = true;
                        break;
                    }
                }
            }
            break;
        }
    }

    if !data_channel && !ended_at_prompt && !output.is_empty() && !output.ends_with(b"\n") {
        if let Some(pos) = output.iter().rposition(|&b| b == b'\n') {
            output.truncate(pos + 1);
        } else {
            output.clear();
        }
    }

    let raw = String::from_utf8_lossy(&output);
    let cleaned = strip_ansi(&raw);

    if data_channel {
        if let Some(s) = sentinel
            && let Some(pos) = cleaned.find(s)
        {
            return cleaned[..pos].trim_end().to_string();
        }

        return cleaned.trim_end().to_string();
    }

    if !bus.uart_ctrl.tx_is_empty() {
        strip_kernel_log(&cleaned).trim_end().to_string()
    } else {
        cleaned.trim_end().to_string()
    }
}

// TODO: evaluate if it's possible to refactor to a solution that filter directly the kernel log on the uart
fn strip_kernel_log(s: &str) -> String {
    s.lines()
        .filter(|line| {
            let t = line.trim_start();

            !(t.starts_with("---[")
                || t.starts_with('[') && t.contains("] ") && {
                    let after = &t[1..];
                    after
                        .find(']')
                        .map(|i| {
                            after[..i]
                                .trim()
                                .bytes()
                                .all(|b| b.is_ascii_digit() || b == b'.')
                        })
                        .unwrap_or(false)
                })
        })
        .collect::<Vec<_>>()
        .join("\n")
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
