use machine::machine_bus::MachineBus;
use riscv_core::{Hart, StepResult};
use wasi::clocks::monotonic_clock;
use wasi::io::poll;
use wasi::io::streams::{InputStream, StreamError};

pub fn run(bus: &mut MachineBus, hart: &mut Hart) {
    eprintln!("[capsulev-wasi] Press Ctrl-C to exit.");

    let stdin = wasi::cli::stdin::get_stdin();
    let mut esc_state: u8 = 0;

    const POLL_INTERVAL: u64 = 8192;
    let mut steps: u64 = 0;

    loop {
        bus.clint.advance(POLL_INTERVAL);
        bus.poll(hart);
        poll_stdin(bus, &stdin, &mut esc_state);

        match hart.run(bus, POLL_INTERVAL) {
            StepResult::Ok => {}
            StepResult::Trap(cause) => {
                eprintln!(
                    "[capsulev-wasi] unhandled trap {:?} at pc={:#x}",
                    cause, hart.regs.pc
                );
                break;
            }
            StepResult::Halt => {
                eprintln!("[capsulev-wasi] halt after {} steps", steps);
                break;
            }
        }
        steps += POLL_INTERVAL;
    }
}

fn poll_stdin(bus: &mut MachineBus, stdin: &InputStream, esc_state: &mut u8) {
    let pollable = stdin.subscribe();
    let timer = monotonic_clock::subscribe_duration(0);
    let ready = poll::poll(&[&pollable, &timer]);

    if !ready.contains(&0) {
        return;
    }

    match stdin.read(64) {
        Ok(bytes) => {
            for b in bytes {
                match *esc_state {
                    0 if b == 0x1b => *esc_state = 1,
                    1 if b == b'[' => *esc_state = 2,
                    1 => {
                        *esc_state = 0;
                        bus.uart.push_rx(0x1b);
                        bus.uart.push_rx(b);
                    }
                    2 => {
                        if b.is_ascii_alphabetic() || b == b'~' {
                            *esc_state = 0;
                        }
                    }
                    _ => bus.uart.push_rx(b),
                }
            }
        }
        Err(StreamError::Closed) => std::process::exit(0),
        Err(StreamError::LastOperationFailed(_)) => {}
    }
}
