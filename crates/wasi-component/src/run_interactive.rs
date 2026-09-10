use machine::machine_bus::MachineBus;
use riscv_core::{Hart, StepResult};
use wasi::clocks::monotonic_clock;
use wasi::io::poll;
use wasi::io::streams::{InputStream, StreamError};

const KERNEL_PANIC: &[u8] = b"Kernel panic";
const OUTPUT_HOLD_CYCLES: u32 = 3;

const MAX_PENDING_BYTES: usize = 64 * 1024;

pub fn run(bus: &mut MachineBus, hart: &mut Hart) {
    let stdin = wasi::cli::stdin::get_stdin();

    const POLL_INTERVAL_ACTIVE: u64 = 131_072;
    const POLL_INTERVAL_IDLE: u64 = 8192;
    const POLL_INTERVAL_NET: u64 = 4096;
    const IDLE_TIMEOUT_NS: u64 = 50_000_000;
    const IDLE_THRESHOLD: u32 = 32;

    let mut idle_ticks = 0u32;
    let mut pending: Vec<u8> = Vec::new();
    let mut panic_scan_tail: Vec<u8> = Vec::new();
    let mut panicked = false;
    let mut hold_cycles = 0u32;
    let mut active_ticks = 0u32;

    let stdin_pollable = stdin.subscribe();

    loop {
        let interval = if bus.net_rx_pending() {
            POLL_INTERVAL_NET
        } else if idle_ticks > IDLE_THRESHOLD {
            POLL_INTERVAL_IDLE
        } else {
            POLL_INTERVAL_ACTIVE
        };

        bus.clint.advance_by_instructions(interval);
        bus.poll(hart);

        if poll_stdin(bus, &stdin, &stdin_pollable) {
            bus.poll(hart);
            idle_ticks = 0;
            active_ticks = 512;
        }

        let bytes = bus.uart.drain_tx();
        if !bytes.is_empty() {
            panicked = saw_kernel_panic(&mut panic_scan_tail, &bytes);

            pending.extend_from_slice(&bytes);
            hold_cycles = 0;

            if !panicked && pending.len() >= MAX_PENDING_BYTES {
                flush_pending(&mut pending);
            }
        }

        let stderr_bytes = bus.uart_stderr.drain_tx();
        if !stderr_bytes.is_empty() {
            let stderr = wasi::cli::stderr::get_stderr();
            let _ = stderr.write(&stderr_bytes);
            let _ = stderr.flush();
        }

        if panicked {
            break;
        }

        if !pending.is_empty() {
            hold_cycles += 1;
            if hold_cycles > OUTPUT_HOLD_CYCLES {
                let stdout = wasi::cli::stdout::get_stdout();
                let _ = stdout.write(&pending);
                let _ = stdout.flush();
                pending.clear();
            }
        }

        if active_ticks > 0 {
            active_ticks = active_ticks.saturating_sub(1);
        }

        if hart.is_waiting {
            idle_ticks += 1;
            hart.is_waiting = false;

            if idle_ticks > IDLE_THRESHOLD && active_ticks == 0 && !bus.has_pending_io() {
                flush_pending(&mut pending);

                let before = monotonic_clock::now();
                let stdin_ready = stdin.subscribe();
                let timeout = monotonic_clock::subscribe_duration(IDLE_TIMEOUT_NS);
                poll::poll(&[&stdin_ready, &timeout]);

                bus.clint
                    .advance_by_nanos(monotonic_clock::now().saturating_sub(before));

                if stdin_ready.ready() {
                    if let Ok(bytes) = stdin.read(64) {
                        for &b in &bytes {
                            bus.uart.push_rx(b);
                        }
                    }
                    idle_ticks = 0;
                    active_ticks = 512;
                    bus.poll(hart);
                }
            }
        } else {
            idle_ticks = 0;
        }

        match hart.run(bus, interval) {
            StepResult::Ok => {}
            StepResult::Trap(cause) => {
                flush_pending(&mut pending);
                eprintln!(
                    "\r\n[vpod-wasi] unhandled trap {:?} at pc={:#x}",
                    cause, hart.regs.pc
                );
                break;
            }
            StepResult::Halt => {
                pending.extend_from_slice(&bus.uart.drain_tx());
                flush_pending(&mut pending);

                let stderr_bytes = bus.uart_stderr.drain_tx();
                if !stderr_bytes.is_empty() {
                    let stderr = wasi::cli::stderr::get_stderr();
                    let _ = stderr.write(&stderr_bytes);
                    let _ = stderr.flush();
                }

                break;
            }
        }
    }
}

fn saw_kernel_panic(tail: &mut Vec<u8>, bytes: &[u8]) -> bool {
    let overlap = KERNEL_PANIC.len() - 1;

    tail.extend_from_slice(bytes);
    let found = tail.windows(KERNEL_PANIC.len()).any(|w| w == KERNEL_PANIC);

    if tail.len() > overlap {
        let keep_from = tail.len() - overlap;
        tail.drain(..keep_from);
    }

    found
}

fn flush_pending(pending: &mut Vec<u8>) {
    if pending.is_empty() {
        return;
    }
    let stdout = wasi::cli::stdout::get_stdout();
    let _ = stdout.write(pending);
    let _ = stdout.flush();
    pending.clear();
}

fn poll_stdin(bus: &mut MachineBus, stdin: &InputStream, pollable: &poll::Pollable) -> bool {
    if !pollable.ready() {
        return false;
    }

    match stdin.read(64) {
        Ok(bytes) if !bytes.is_empty() => {
            for &b in &bytes {
                bus.uart.push_rx(b);
            }
            true
        }
        Ok(_) => false,
        Err(StreamError::Closed) => std::process::exit(0),
        Err(StreamError::LastOperationFailed(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_panic_inside_one_drain() {
        let mut tail = Vec::new();
        assert!(saw_kernel_panic(&mut tail, b"before Kernel panic after"));
    }

    #[test]
    fn finds_a_panic_split_across_two_drains() {
        let mut tail = Vec::new();

        assert!(!saw_kernel_panic(&mut tail, b"end of a line Kernel pa"));
        assert!(saw_kernel_panic(&mut tail, b"nic - not syncing"));
    }

    #[test]
    fn does_not_join_bytes_that_never_spelled_a_panic() {
        let mut tail = Vec::new();

        assert!(!saw_kernel_panic(&mut tail, b"Kernel pan"));
        assert!(!saw_kernel_panic(&mut tail, b"try shelf"));
    }

    #[test]
    fn keeps_the_carried_tail_bounded_under_a_long_dump() {
        let mut tail = Vec::new();

        for _ in 0..64 {
            assert!(!saw_kernel_panic(&mut tail, &[b'x'; 4096]));
        }

        assert_eq!(tail.len(), KERNEL_PANIC.len() - 1);
    }
}
