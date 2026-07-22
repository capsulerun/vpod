use machine::machine_bus::MachineBus;
use riscv_core::{Hart, StepResult};
use wasi::clocks::monotonic_clock;
use wasi::io::poll;
use wasi::io::streams::{InputStream, StreamError};

const KERNEL_PANIC: &[u8] = b"Kernel panic";
const OUTPUT_HOLD_CYCLES: u32 = 3;

pub fn run(bus: &mut MachineBus, hart: &mut Hart) {
    let stdin = wasi::cli::stdin::get_stdin();

    const POLL_INTERVAL_ACTIVE: u64 = 131_072;
    const POLL_INTERVAL_IDLE: u64 = 8192;
    const POLL_INTERVAL_NET: u64 = 4096;
    const IDLE_TIMEOUT_NS: u64 = 50_000_000;
    const IDLE_THRESHOLD: u32 = 32;

    let mut idle_ticks = 0u32;
    let mut pending: Vec<u8> = Vec::new();
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
            pending.extend_from_slice(&bytes);
            hold_cycles = 0;
        }

        let stderr_bytes = bus.uart_stderr.drain_tx();
        if !stderr_bytes.is_empty() {
            let stderr = wasi::cli::stderr::get_stderr();
            let _ = stderr.write(&stderr_bytes);
            let _ = stderr.flush();
        }

        if !pending.is_empty() {
            if pending
                .windows(KERNEL_PANIC.len())
                .any(|w| w == KERNEL_PANIC)
            {
                break;
            }

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
                let stdin_ready = stdin.subscribe();
                let timeout = monotonic_clock::subscribe_duration(IDLE_TIMEOUT_NS);
                poll::poll(&[&stdin_ready, &timeout]);

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
            StepResult::Halt => break,
        }
    }
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
