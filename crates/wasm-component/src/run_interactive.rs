use machine::machine_bus::MachineBus;
use riscv_core::{Hart, StepResult};
use wasi::clocks::monotonic_clock;
use wasi::io::poll;
use wasi::io::streams::{InputStream, StreamError};

pub fn run(bus: &mut MachineBus, hart: &mut Hart) {
    let stdin = wasi::cli::stdin::get_stdin();

    const POLL_INTERVAL: u64 = 8192;
    let mut steps: u64 = 0;

    loop {
        bus.clint.advance(POLL_INTERVAL);
        bus.poll(hart);

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

        flush_output(bus);
        poll_stdin(bus, &stdin);
        steps += POLL_INTERVAL;
    }

    flush_output(bus);
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

fn flush_output(bus: &mut MachineBus) {
    let bytes = bus.uart.drain_tx();
    if bytes.is_empty() {
        return;
    }
    let stdout = wasi::cli::stdout::get_stdout();
    let _ = stdout.write(&bytes);
    let _ = stdout.flush();
}
