use machine::machine_bus::MachineBus;
use riscv_core::{Hart, StepResult};
use wasi::cli::stdin;
use wasi::io::streams::StreamError;

pub fn run(bus: &mut MachineBus, hart: &mut Hart) {
    let stdin = stdin::get_stdin();
    let mut script: Vec<u8> = Vec::new();

    loop {
        match stdin.read(4096) {
            Ok(bytes) if !bytes.is_empty() => script.extend_from_slice(&bytes),
            Ok(_) | Err(StreamError::Closed) | Err(_) => break,
        }
    }

    for b in &script {
        bus.uart.push_rx(*b);
    }

    if script.last() != Some(&b'\n') {
        bus.uart.push_rx(b'\n');
    }

    for b in b"CAPSULEV_EOF\n" {
        bus.uart.push_rx(*b);
    }

    const POLL_INTERVAL: u64 = 8192;
    loop {
        bus.clint.advance_by_instructions(POLL_INTERVAL);
        bus.poll(hart);

        if hart.is_waiting && !bus.has_pending_io() {
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }

        match hart.run(bus, POLL_INTERVAL) {
            StepResult::Ok => {}
            StepResult::Trap(cause) => {
                eprintln!(
                    "[capsulev-wasi-worker] trap {:?} at pc={:#x}",
                    cause, hart.regs.pc
                );
                std::process::exit(1);
            }
            StepResult::Halt => break,
        }
    }
}
