use machine::machine_bus::MachineBus;
use riscv_core::{Hart, StepResult};

const POLL_INTERVAL: u64 = 8192;

pub fn wait_for_prompt(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8]) {
    let mut buffer = Vec::new();
    let mut idle_count = 0;

    loop {
        bus.clint.advance_by_instructions(POLL_INTERVAL);
        bus.poll(hart);

        let output = bus.console.take_tx_buffer();
        if !output.is_empty() {
            buffer.extend_from_slice(&output);
            idle_count = 0;

            if buffer.windows(prompt.len()).any(|w| w == prompt) {
                return;
            }
        } else {
            idle_count += 1;
            if idle_count > 20 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }

        if hart.is_waiting && !bus.has_pending_io() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        match hart.run(bus, POLL_INTERVAL) {
            StepResult::Ok => {}
            StepResult::Trap(cause) => {
                eprintln!("[session] trap during startup: {:?}", cause);
                std::process::exit(1);
            }
            StepResult::Halt => {
                eprintln!("[session] unexpected halt");
                std::process::exit(1);
            }
        }
    }
}

pub fn capture_output_until_prompt(bus: &mut MachineBus, hart: &mut Hart, prompt: &[u8]) -> String {
    let mut output = Vec::new();
    let mut idle_count = 0;
    let mut no_output_count = 0;

    loop {
        bus.clint.advance_by_instructions(POLL_INTERVAL);
        bus.poll(hart);

        let tx = bus.console.take_tx_buffer();
        if !tx.is_empty() {
            output.extend_from_slice(&tx);
            idle_count = 0;
            no_output_count = 0;

            if output.ends_with(prompt) {
                output.truncate(output.len() - prompt.len());
                break;
            }
        } else {
            no_output_count += 1;
            if no_output_count > 100 {
                break;
            }
        }

        if hart.is_waiting && !bus.has_pending_io() {
            idle_count += 1;
            if idle_count > 10 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        } else {
            idle_count = 0;
        }

        match hart.run(bus, POLL_INTERVAL) {
            StepResult::Ok => {}
            StepResult::Trap(cause) => {
                eprintln!("[session] trap: {:?}", cause);
                break;
            }
            StepResult::Halt => break,
        }
    }

    String::from_utf8_lossy(&output).trim_end().to_string()
}
