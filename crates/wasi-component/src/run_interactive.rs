use machine::machine_bus::MachineBus;
use riscv_core::{Hart, StepResult};
use wasi::clocks::monotonic_clock;
use wasi::io::poll;
use wasi::io::streams::{InputStream, StreamError};

pub fn run(bus: &mut MachineBus, hart: &mut Hart) {
    let stdin = wasi::cli::stdin::get_stdin();

    const POLL_INTERVAL_ACTIVE: u64 = 32768;
    const POLL_INTERVAL_IDLE: u64 = 1024;
    const POLL_INTERVAL_NET: u64 = 4096;
    const IDLE_TIMEOUT_NS: u64 = 10_000_000;

    let mut idle_ticks = 0u32;

    loop {
        let interval = if bus.net_rx_pending() {
            POLL_INTERVAL_NET
        } else if idle_ticks > 4 {
            POLL_INTERVAL_IDLE
        } else {
            POLL_INTERVAL_ACTIVE
        };

        bus.clint.advance_by_instructions(interval);
        bus.poll(hart);

        if poll_stdin(bus, &stdin) {
            bus.poll(hart);
            idle_ticks = 0;
        }

        flush_console(bus);

        if hart.is_waiting {
            idle_ticks += 1;
            hart.is_waiting = false;

            if !bus.has_pending_io() {
                let stdin_ready = stdin.subscribe();
                let timeout = monotonic_clock::subscribe_duration(IDLE_TIMEOUT_NS);
                poll::poll(&[&stdin_ready, &timeout]);
            }
        } else {
            idle_ticks = 0;
        }

        match hart.run(bus, interval) {
            StepResult::Ok => {}
            StepResult::Trap(cause) => {
                eprintln!(
                    "\r\n[vpod-wasi] unhandled trap {:?} at pc={:#x}",
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
