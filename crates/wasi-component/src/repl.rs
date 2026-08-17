use machine::machine_bus::MachineBus;
use riscv_core::{Hart, StepResult};
use wasi::clocks::monotonic_clock;
use wasi::clocks::wall_clock;
use wasi::io::poll;

const STEP: u64 = 8192;
const RUN_STEP: u64 = 524_288;

const MAX_TIMER_WARP_NS: u64 = 100_000_000;
const NET_YIELD_NS: u64 = 5_000_000; // 5 ms

const GRACE_STEPS: u32 = 2000;

const SEED_BINARY: &str = "/usr/lib/vpod/vpod-seed-entropy";
const SEED_BYTES: u64 = 32;

pub fn reseed_fragment() -> String {
    let seed = wasi::random::random::get_random_bytes(SEED_BYTES);

    let mut hex = String::with_capacity(seed.len() * 2);
    for byte in &seed {
        hex.push_str(&format!("{byte:02x}"));
    }

    format!("{SEED_BINARY} {hex}")
}

fn warn_if_unseeded(complaint: &[u8]) {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    if complaint.is_empty() || WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }

    eprintln!(
        "[vpod] warning: could not reseed the guest random pool, so every sandbox from \
         this snapshot will produce identical random bytes. Re-pull the snapshot so it \
         carries {SEED_BINARY}. ({})",
        String::from_utf8_lossy(complaint).trim()
    );
}

pub fn reseed(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8]) {
    let cmd = format!("{} 2>/dev/ttyS1\n", reseed_fragment());

    bus.uart_stderr.drain_tx();

    for byte in cmd.bytes() {
        bus.uart.push_rx(byte);
    }

    wait_for_prompt(bus, hart, prompt);

    bus.uart.drain_tx();
    warn_if_unseeded(&bus.uart_stderr.drain_tx());
    bus.uart_ctrl.drain_tx();
}

pub fn sync_clock(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8]) {
    let now = wall_clock::now();
    let date_cmd = format!(
        "date -s @{} >/dev/null; {} 2>/dev/ttyS1\n",
        now.seconds,
        reseed_fragment()
    );

    bus.uart_stderr.drain_tx();

    for byte in date_cmd.bytes() {
        bus.uart.push_rx(byte);
    }

    wait_for_prompt(bus, hart, prompt);

    bus.uart.drain_tx();
    warn_if_unseeded(&bus.uart_stderr.drain_tx());
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
        "__ec() {{ printf \"\\x$(printf %02x $1)\" >/dev/ttyS2; }}; export PS2=''; export PS1='$(__ec $?){}'; trap '__ec $?' EXIT\n",
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

pub fn settle(bus: &mut MachineBus, hart: &mut Hart, wall_ns: u64) {
    let deadline = monotonic_clock::now() + wall_ns;

    while monotonic_clock::now() < deadline {
        if hart.is_waiting {
            hart.is_waiting = false;
        }

        bus.clint.advance_by_instructions(STEP);
        bus.poll(hart);

        if let StepResult::Trap(_) | StepResult::Halt = hart.run(bus, STEP) {
            return;
        }

        bus.uart.drain_tx();
    }
}

pub fn drain_ctrl_with_grace(bus: &mut MachineBus, hart: &mut Hart) -> Vec<u8> {
    for _ in 0..GRACE_STEPS {
        let bytes = bus.uart_ctrl.drain_tx();
        if !bytes.is_empty() {
            return bytes;
        }

        if hart.is_waiting {
            hart.is_waiting = false;
        }

        bus.clint.advance_by_instructions(STEP);
        bus.poll(hart);

        if let StepResult::Trap(_) | StepResult::Halt = hart.run(bus, STEP) {
            break;
        }
    }

    bus.uart_ctrl.drain_tx()
}

pub fn wait_for_prompt(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8]) -> bool {
    let mut buffer = Vec::new();

    for _ in 0..500_000u32 {
        if hart.is_waiting {
            hart.is_waiting = false;
        }

        bus.clint.advance_by_instructions(STEP);
        bus.poll(hart);

        match hart.run(bus, STEP) {
            StepResult::Ok => {}
            StepResult::Trap(_) | StepResult::Halt => return false,
        }

        let output = bus.uart.drain_tx();
        if !output.is_empty() {
            buffer.extend_from_slice(&output);
            if buffer.ends_with(prompt) {
                return true;
            }
        }
    }

    false
}

pub fn absorb_stray_prompt(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8]) {
    let mut buffer = Vec::new();

    for _ in 0..GRACE_STEPS {
        if hart.is_waiting {
            hart.is_waiting = false;
        }

        bus.clint.advance_by_instructions(STEP);
        bus.poll(hart);

        if let StepResult::Trap(_) | StepResult::Halt = hart.run(bus, STEP) {
            return;
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

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum SliceOutcome {
    Finished,
    TimedOut,
    Yielded,
}

pub struct ExecState {
    output: Vec<u8>,
    ended_at_prompt: bool,
    deadline: u64,
}

impl ExecState {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            output: Vec::new(),
            ended_at_prompt: false,
            deadline: monotonic_clock::now() + timeout_secs * 1_000_000_000,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run_slice(
    bus: &mut MachineBus,
    hart: &mut Hart,
    prompt: &[u8],
    stop_on_ctrl: bool,
    sentinel: Option<&str>,
    data_channel: bool,
    state: &mut ExecState,
    slice_nanos: u64,
) -> SliceOutcome {
    let slice_deadline = monotonic_clock::now().saturating_add(slice_nanos);

    loop {
        let now = monotonic_clock::now();
        if now >= state.deadline {
            return SliceOutcome::TimedOut;
        }
        if now >= slice_deadline {
            return SliceOutcome::Yielded;
        }

        if hart.is_waiting {
            hart.is_waiting = false;

            if !bus.has_pending_io() {
                if matches!(bus.clint.nanos_until_timer(), Some(ns) if ns <= MAX_TIMER_WARP_NS) {
                    bus.clint.fast_forward_to_timer();
                    bus.poll(hart);
                } else {
                    let before = monotonic_clock::now();
                    let timeout = monotonic_clock::subscribe_duration(NET_YIELD_NS);
                    poll::poll(&[&timeout]);

                    let idle_ns = monotonic_clock::now().saturating_sub(before);
                    bus.clint.advance_by_nanos(idle_ns);
                }
            }
        }

        let step = if bus.net_rx_pending() { STEP } else { RUN_STEP };
        bus.clint.advance_by_instructions(step);
        bus.poll(hart);

        match hart.run_until_wait(bus, step) {
            StepResult::Ok => {}
            StepResult::Trap(_) | StepResult::Halt => {
                if stop_on_ctrl && !bus.uart_ctrl.tx_is_empty() {
                    bus.uart.drain_tx();
                }
                return SliceOutcome::Finished;
            }
        }

        let tx = if data_channel {
            bus.uart_data.drain_tx()
        } else {
            bus.uart.drain_tx()
        };

        if !tx.is_empty() {
            state.output.extend_from_slice(&tx);

            if !data_channel && state.output.ends_with(prompt) {
                state.output.truncate(state.output.len() - prompt.len());
                state.ended_at_prompt = true;
                return SliceOutcome::Finished;
            }

            if let Some(s) = sentinel
                && let Ok(text) = std::str::from_utf8(&state.output)
                && text.contains(s)
            {
                return SliceOutcome::Finished;
            }
        }

        if !data_channel && stop_on_ctrl && !bus.uart_ctrl.tx_is_empty() {
            for _ in 0..1024 {
                bus.clint.advance_by_instructions(STEP);
                bus.poll(hart);

                match hart.run(bus, STEP) {
                    StepResult::Ok => {}
                    StepResult::Trap(_) | StepResult::Halt => break,
                }

                let extra = bus.uart.drain_tx();
                if !extra.is_empty() {
                    state.output.extend_from_slice(&extra);

                    if state.output.ends_with(prompt) {
                        state.output.truncate(state.output.len() - prompt.len());
                        state.ended_at_prompt = true;
                        break;
                    }
                }
            }
            return SliceOutcome::Finished;
        }
    }
}

pub fn finish_output(
    bus: &MachineBus,
    sentinel: Option<&str>,
    data_channel: bool,
    state: ExecState,
) -> String {
    let mut output = state.output;

    if !data_channel && !state.ended_at_prompt && !output.is_empty() && !output.ends_with(b"\n") {
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

pub fn capture_output(
    bus: &mut MachineBus,
    hart: &mut Hart,
    prompt: &[u8],
    timeout_secs: u64,
    stop_on_ctrl: bool,
    sentinel: Option<&str>,
    data_channel: bool,
) -> String {
    let mut state = ExecState::new(timeout_secs);

    while run_slice(
        bus,
        hart,
        prompt,
        stop_on_ctrl,
        sentinel,
        data_channel,
        &mut state,
        u64::MAX,
    ) == SliceOutcome::Yielded
    {}

    finish_output(bus, sentinel, data_channel, state)
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
