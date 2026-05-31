use machine::machine_bus::MachineBus;
use riscv_core::{Hart, StepResult};
use wasi::clocks::monotonic_clock;
use wasi::io::poll;
use wasi::io::streams::{InputStream, StreamError};

pub fn run(bus: &mut MachineBus, hart: &mut Hart) {
    let stdin = wasi::cli::stdin::get_stdin();

    const POLL_INTERVAL: u64 = 8192;

    // let mut steps: u64 = 0;
    let mut consecutive_idle = 0u32;
    let mut panic_detected = false;

    loop {
        bus.clint.advance(POLL_INTERVAL);
        bus.poll(hart);

        poll_stdin(bus, &stdin);

        if hart.is_waiting && bus.has_pending_io() {
            hart.is_waiting = false;
        }

        if hart.is_waiting && !bus.has_pending_io() {
            if flush_output(bus, &mut panic_detected) {
                break; // Clean exit on init exit
            }

            if flush_console_output(bus, &mut panic_detected) {
                break; // Clean exit on init exit
            }
            consecutive_idle += 1;

            let sleep_ms = if consecutive_idle < 5 {
                1
            } else if consecutive_idle < 20 {
                5
            } else {
                10
            };

            std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
            continue;
        }

        consecutive_idle = 0;

        match hart.run(bus, POLL_INTERVAL) {
            StepResult::Ok => {}
            StepResult::Trap(cause) => {
                eprintln!(
                    "\r\n[capsulev-wasi] unhandled trap {:?} at pc={:#x}",
                    cause, hart.regs.pc
                );
                break;
            }
            StepResult::Halt => break,
        }

        if flush_output(bus, &mut panic_detected) {
            break; // Clean exit on init exit
        }
        if flush_console_output(bus, &mut panic_detected) {
            break; // Clean exit on init exit
        }

        // steps += POLL_INTERVAL;
    }

    flush_output(bus, &mut panic_detected);
    flush_console_output(bus, &mut panic_detected);
}

fn poll_stdin(bus: &mut MachineBus, stdin: &InputStream) {
    let pollable = stdin.subscribe();
    let timer = monotonic_clock::subscribe_duration(0);
    let ready = poll::poll(&[&pollable, &timer]);

    if !ready.contains(&0) {
        return;
    }

    match stdin.read(64) {
        Ok(bytes) => {
            for b in bytes {
                bus.uart.push_rx(b);
            }
        }
        Err(StreamError::Closed) => std::process::exit(0),
        Err(StreamError::LastOperationFailed(_)) => {}
    }
}

fn flush_output(bus: &mut MachineBus, panic_detected: &mut bool) -> bool {
    let bytes = bus.uart.drain_tx();
    if bytes.is_empty() {
        return false;
    }

    if let Ok(text) = std::str::from_utf8(&bytes) {
        if text.contains("Kernel panic") {
            *panic_detected = true;
            std::process::exit(0);
        }

        if *panic_detected {
            return true;
        }
    }

    let stdout = wasi::cli::stdout::get_stdout();
    let _ = stdout.write(&bytes);
    let _ = stdout.flush();
    false
}

fn flush_console_output(bus: &mut MachineBus, panic_detected: &mut bool) -> bool {
    let bytes = bus.console.take_tx_buffer();
    if bytes.is_empty() {
        return false;
    }

    if let Ok(text) = std::str::from_utf8(&bytes) {
        if text.contains("Kernel panic") {
            *panic_detected = true;
            std::process::exit(0);
        }

        if *panic_detected {
            return true;
        }
    }

    let stdout = wasi::cli::stdout::get_stdout();
    let _ = stdout.write(&bytes);
    let _ = stdout.flush();
    false
}
