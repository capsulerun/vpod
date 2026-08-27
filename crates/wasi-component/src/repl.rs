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

fn reseed_fragment() -> String {
    let seed = wasi::random::random::get_random_bytes(SEED_BYTES);

    let mut hex = String::with_capacity(seed.len() * 2);
    for byte in &seed {
        hex.push_str(&format!("{byte:02x}"));
    }
    let shell_seed = wasi::random::random::get_random_u64() as u32;

    format!("RANDOM={shell_seed}; {SEED_BINARY} {hex} 2>/dev/ttyS1")
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

fn run_quietly(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8], cmd: &str) -> Vec<u8> {
    bus.uart_stderr.drain_tx();

    for byte in cmd.bytes() {
        bus.uart.push_rx(byte);
    }
    bus.uart.push_rx(b'\n');

    wait_for_prompt(bus, hart, prompt);

    bus.uart.drain_tx();
    let complaint = bus.uart_stderr.drain_tx();
    bus.uart_ctrl.drain_tx();

    complaint
}

pub fn reseed(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8]) {
    let complaint = run_quietly(bus, hart, prompt, &reseed_fragment());
    warn_if_unseeded(&complaint);
}

pub fn sync_clock(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8]) {
    let now = wall_clock::now();
    run_quietly(
        bus,
        hart,
        prompt,
        &format!("date -s @{} >/dev/null", now.seconds),
    );
}

pub fn sync_clock_and_reseed(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8]) {
    let now = wall_clock::now();
    let cmd = format!("date -s @{} >/dev/null; {}", now.seconds, reseed_fragment());

    let complaint = run_quietly(bus, hart, prompt, &cmd);
    warn_if_unseeded(&complaint);
}

pub fn shell_init(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8]) {
    sync_clock_and_reseed(bus, hart, prompt);

    for byte in b"stty -echo\n" {
        bus.uart.push_rx(*byte);
    }

    wait_for_prompt(bus, hart, prompt);
    bus.uart.drain_tx();

    let init_cmd = format!(
        "__ec() {{ printf \"\\x$(printf %02x $1)\" >/dev/ttyS2; }}; export PS2=''; \
         export PS1='$(__ec $?){}'; export -n PS1; trap '__ec $?' EXIT\n",
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

fn deadline_from(timeout_secs: u64, now: u64) -> u64 {
    if timeout_secs == 0 {
        u64::MAX
    } else {
        now.saturating_add(timeout_secs.saturating_mul(1_000_000_000))
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
    stderr: Vec<u8>,
    ended_at_prompt: bool,
    deadline: u64,
    tty: bool,
}

impl ExecState {
    pub fn new(timeout_secs: u64) -> Self {
        Self::with_mode(timeout_secs, false)
    }

    pub fn with_mode(timeout_secs: u64, tty: bool) -> Self {
        let deadline = deadline_from(timeout_secs, monotonic_clock::now());

        Self {
            output: Vec::new(),
            stderr: Vec::new(),
            ended_at_prompt: false,
            deadline,
            tty,
        }
    }

    pub fn is_terminal(&self) -> bool {
        self.tty
    }

    pub fn absorb_stderr(&mut self, bytes: &[u8]) {
        self.stderr.extend_from_slice(bytes);
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

pub fn drain_output(state: &mut ExecState, prompt: &[u8]) -> String {
    let boundary = if state.tty {
        tty_boundary(&state.output, prompt)
    } else {
        safe_boundary(&state.output)
    };

    if boundary == 0 {
        return String::new();
    }

    let chunk: Vec<u8> = state.output.drain(..boundary).collect();
    let text = String::from_utf8_lossy(&chunk);

    if state.tty {
        strip_kernel_log(&text)
    } else {
        strip_kernel_log(&strip_ansi(&text))
    }
}

fn tty_boundary(buf: &[u8], prompt: &[u8]) -> usize {
    let mut end = buf.len();

    if end > 0 && buf[end - 1] == b'\r' {
        end -= 1;
    }

    end = match std::str::from_utf8(&buf[..end]) {
        Ok(_) => end,
        Err(broken) if broken.error_len().is_none() => broken.valid_up_to(),
        Err(_) => end,
    };

    if let Some(escape) = buf[..end].iter().rposition(|&byte| byte == 0x1b) {
        let tail = &buf[escape..end];
        let complete = match tail.len() {
            0 | 1 => false,
            _ if tail[1] == b'[' => tail[2..].iter().any(|b| b.is_ascii_alphabetic()),
            _ => true,
        };
        if !complete {
            end = escape;
        }
    }

    for n in (1..=prompt.len().min(end)).rev() {
        if buf[end - n..end] == prompt[..n] {
            end -= n;
            break;
        }
    }

    end
}

pub fn drain_stderr(state: &mut ExecState) -> String {
    let boundary = safe_boundary(&state.stderr);
    if boundary == 0 {
        return String::new();
    }

    let chunk: Vec<u8> = state.stderr.drain(..boundary).collect();
    String::from_utf8_lossy(&chunk).into_owned()
}

pub fn finish_stderr(state: &ExecState) -> String {
    String::from_utf8_lossy(&state.stderr).into_owned()
}

fn safe_boundary(buf: &[u8]) -> usize {
    match buf.iter().rposition(|&byte| byte == b'\n') {
        Some(position) => position + 1,
        None => 0,
    }
}

pub fn finish_output(
    bus: &MachineBus,
    sentinel: Option<&str>,
    data_channel: bool,
    state: ExecState,
    trim: bool,
) -> String {
    let mut output = state.output;

    if !state.tty
        && !data_channel
        && !state.ended_at_prompt
        && !output.is_empty()
        && !output.ends_with(b"\n")
    {
        output.truncate(safe_boundary(&output));
    }

    let raw = String::from_utf8_lossy(&output);
    let cleaned = if state.tty {
        raw.to_string()
    } else {
        strip_ansi(&raw)
    };

    if data_channel {
        if let Some(s) = sentinel
            && let Some(pos) = cleaned.find(s)
        {
            return cleaned[..pos].trim_end().to_string();
        }

        return cleaned.trim_end().to_string();
    }

    let filtered = if !bus.uart_ctrl.tx_is_empty() {
        strip_kernel_log(&cleaned)
    } else {
        cleaned
    };

    if state.tty || !trim {
        filtered
    } else {
        filtered.trim_end().to_string()
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

    finish_output(bus, sentinel, data_channel, state, true)
}

// TODO: evaluate if it's possible to refactor to a solution that filter directly the kernel log on the uart
fn strip_kernel_log(s: &str) -> String {
    let mut stripped = s
        .lines()
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
        .join("\n");

    if s.ends_with('\n') && !stripped.is_empty() {
        stripped.push('\n');
    }

    stripped
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_splits_a_multibyte_character() {
        let buf = "héllo\nwörld".as_bytes();
        let boundary = safe_boundary(buf);

        assert_eq!(&buf[..boundary], "héllo\n".as_bytes());
        assert!(std::str::from_utf8(&buf[..boundary]).is_ok());
        assert!(std::str::from_utf8(&buf[boundary..]).is_ok());
    }

    #[test]
    fn never_splits_an_ansi_escape() {
        let buf = b"done\n\x1b[0";
        assert_eq!(safe_boundary(buf), 5);
        assert_eq!(&buf[..5], b"done\n");
    }

    #[test]
    fn never_splits_a_crlf_pair() {
        let buf = b"a\r\nb\r";
        assert_eq!(safe_boundary(buf), 3);
        assert_eq!(&buf[..3], b"a\r\n");
    }

    #[test]
    fn never_splits_the_prompt_sentinel() {
        let buf = b"out\n\x1fvpo";
        assert_eq!(safe_boundary(buf), 4);
        assert_eq!(&buf[..4], b"out\n");
    }

    #[test]
    fn releases_nothing_without_a_newline() {
        assert_eq!(safe_boundary(b""), 0);
        assert_eq!(safe_boundary(b"no newline here"), 0);
    }

    #[test]
    fn releases_everything_when_it_ends_on_a_newline() {
        assert_eq!(safe_boundary(b"a\n"), 2);
        assert_eq!(safe_boundary(b"a\nb\n"), 4);
    }

    #[test]
    fn matches_the_trim_finish_output_used_to_do_inline() {
        for case in [
            &b"line1\nline2\npartial"[..],
            &b"partial"[..],
            &b"line1\n"[..],
            &b""[..],
        ] {
            let mut expected = case.to_vec();
            if let Some(position) = expected.iter().rposition(|&byte| byte == b'\n') {
                expected.truncate(position + 1);
            } else {
                expected.clear();
            }

            let mut actual = case.to_vec();
            actual.truncate(safe_boundary(&actual));

            assert_eq!(actual, expected, "diverged on {case:?}");
        }
    }

    #[test]
    fn a_chunk_keeps_the_newline_that_separates_it_from_the_next() {
        assert_eq!(strip_kernel_log("a\n"), "a\n");
        assert_eq!(strip_kernel_log("a\nb\n"), "a\nb\n");
        assert_eq!(strip_kernel_log("a"), "a");
        assert_eq!(strip_kernel_log("a\n\n"), "a\n\n");
    }

    #[test]
    fn a_chunk_of_nothing_but_kernel_log_stays_empty() {
        assert_eq!(strip_kernel_log("[    0.123456] booting\n"), "");
    }

    const SENTINEL: &[u8] = b"\x1fvpod\x1f";

    #[test]
    fn a_terminal_releases_a_prompt_that_has_no_newline() {
        assert_eq!(safe_boundary(b">>> "), 0);
        assert_eq!(tty_boundary(b">>> ", SENTINEL), 4);
    }

    #[test]
    fn a_terminal_never_splits_a_multibyte_character() {
        let whole = "héllo".as_bytes();
        assert_eq!(tty_boundary(whole, SENTINEL), whole.len());
        assert_eq!(tty_boundary(&whole[..3], SENTINEL), 3);

        let torn = &whole[..2];
        assert_eq!(tty_boundary(torn, SENTINEL), 1);
        assert!(std::str::from_utf8(&torn[..tty_boundary(torn, SENTINEL)]).is_ok());
    }

    #[test]
    fn a_terminal_never_splits_an_ansi_escape() {
        assert_eq!(tty_boundary(b"red\x1b[31", SENTINEL), 3);
        assert_eq!(tty_boundary(b"red\x1b[31m", SENTINEL), 8);
        assert_eq!(tty_boundary(b"red\x1b", SENTINEL), 3);
    }

    #[test]
    fn a_terminal_never_splits_a_crlf_pair() {
        assert_eq!(tty_boundary(b"a\r", SENTINEL), 1);
        assert_eq!(tty_boundary(b"a\r\n", SENTINEL), 3);
    }

    #[test]
    fn a_terminal_never_leaks_a_partial_prompt_sentinel() {
        assert_eq!(tty_boundary(b"out\x1fvpo", SENTINEL), 3);
        assert_eq!(tty_boundary(b"out\x1f", SENTINEL), 3);
        // A lone 0x1f that is not the start of the sentinel is still held, which
        // costs one byte of latency and never corrupts the stream.
        assert_eq!(tty_boundary(b"out", SENTINEL), 3);
    }

    #[test]
    fn a_terminal_holds_back_nothing_it_does_not_have_to() {
        assert_eq!(tty_boundary(b"", SENTINEL), 0);
        assert_eq!(tty_boundary(b"plain text", SENTINEL), 10);
        assert_eq!(tty_boundary(b"line\n", SENTINEL), 5);
    }

    #[test]
    fn an_invalid_byte_does_not_stall_the_stream_forever() {
        assert_eq!(tty_boundary(b"ok\xff", SENTINEL), 3);
    }

    #[test]
    fn a_zero_timeout_means_no_deadline() {
        // A terminal session ends when the program exits or the caller
        // interrupts it, never on a clock.
        assert_eq!(deadline_from(0, 12_345), u64::MAX);
        assert_eq!(deadline_from(30, 1_000), 1_000 + 30_000_000_000);
    }

    #[test]
    fn a_deadline_saturates_rather_than_wrapping() {
        // Wrapping would put the deadline in the past and time the command out
        // immediately.
        assert_eq!(deadline_from(u64::MAX, 1), u64::MAX);
        assert_eq!(deadline_from(1, u64::MAX), u64::MAX);
    }
}
