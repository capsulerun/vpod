use machine::machine_bus::MachineBus;
use riscv_core::{Hart, StepResult};
use wasi::io::streams::{InputStream, StreamError};

pub fn run(bus: &mut MachineBus, hart: &mut Hart) {
    let stdin = wasi::cli::stdin::get_stdin();

    const POLL_INTERVAL: u64 = 32768;
    const IDLE_THRESHOLD: u32 = 64;

    let mut idle_count = 0u32;

    loop {
        bus.clint.advance(POLL_INTERVAL);
        bus.poll(hart);

        if poll_stdin(bus, &stdin) {
            bus.poll(hart);
            idle_count = 0;
        }

        flush_console(bus);

        if hart.is_waiting {
            if idle_count >= IDLE_THRESHOLD {
                let sleep_ms = if idle_count < IDLE_THRESHOLD + 5 {
                    1
                } else if idle_count < IDLE_THRESHOLD + 20 {
                    5
                } else {
                    10
                };
                std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
                continue;
            }
            hart.is_waiting = false;
            idle_count += 1;
        } else {
            idle_count = 0;
        }

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

        flush_console(bus);
    }

    flush_console(bus);
}

fn poll_stdin(bus: &mut MachineBus, stdin: &InputStream) -> bool {
    let pollable = stdin.subscribe();
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

fn flush_console(bus: &mut MachineBus) {
    let bytes = bus.console.take_tx_buffer();
    if bytes.is_empty() {
        return;
    }

    let stdout = wasi::cli::stdout::get_stdout();
    let _ = stdout.write(&bytes);
    let _ = stdout.flush();
}
