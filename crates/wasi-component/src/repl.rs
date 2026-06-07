use machine::machine_bus::MachineBus;
use riscv_core::{Hart, StepResult};

const POLL_INTERVAL: u64 = 65536;
const MAX_ITERATIONS: u64 = 500_000;

pub fn wait_for_prompt(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8]) {
    let mut buffer = Vec::new();
    let mut iterations: u64 = 0;

    loop {
        iterations += 1;
        if iterations > MAX_ITERATIONS {
            eprintln!("[session] timeout waiting for prompt");
            return;
        }

        bus.clint.advance_by_instructions(POLL_INTERVAL);
        bus.poll(hart);

        let output = bus.uart.drain_tx();
        if !output.is_empty() {
            buffer.extend_from_slice(&output);

            if buffer.windows(prompt.len()).any(|w| w == prompt) {
                return;
            }
        } else if hart.is_waiting {
            hart.is_waiting = false;
        }

        match hart.run(bus, POLL_INTERVAL) {
            StepResult::Ok => {}
            StepResult::Trap(cause) => {
                eprintln!("[session] trap during startup: {:?}", cause);
                return;
            }
            StepResult::Halt => {
                eprintln!("[session] unexpected halt");
                return;
            }
        }
    }
}

pub fn capture_output_until_prompt(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8]) -> String {
    let mut output = Vec::new();
    let mut consecutive_idle = 0u32;
    let mut got_any_output = false;

    loop {
        bus.clint.advance_by_instructions(POLL_INTERVAL);
        bus.poll(hart);

        let tx = bus.uart.drain_tx();
        if !tx.is_empty() {
            output.extend_from_slice(&tx);
            consecutive_idle = 0;
            got_any_output = true;

            if let Some(pos) = find_subsequence(&output, prompt) {
                output.truncate(pos);
                break;
            }
        } else {
            consecutive_idle += 1;

            if got_any_output && consecutive_idle > 100_000 {
                break;
            }

            if consecutive_idle > 500_000 {
                break;
            }

            if hart.is_waiting {
                hart.is_waiting = false;
            }
        }

        match hart.run(bus, POLL_INTERVAL) {
            StepResult::Ok => {}
            StepResult::Trap(cause) => {
                eprintln!("[session] trap: {:?}", cause);
                break;
            }
            StepResult::Halt => break,
        }
    }

    if let Some(pos) = output.iter().rposition(|&b| b == b'\n') {
        output.truncate(pos + 1);
    }

    let raw = String::from_utf8_lossy(&output);
    let cleaned = strip_ansi(&raw);
    match cleaned.find('\n') {
        Some(pos) => cleaned[pos + 1..].trim_end().to_string(),
        None => cleaned.trim_end().to_string(),
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
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
