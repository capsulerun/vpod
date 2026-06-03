use machine::machine_bus::MachineBus;
use riscv_core::{Hart, StepResult};

use crate::api::session::SESSION_MANAGER;
use crate::exports::vpod::sandbox::executor::{ExecutionResult, Guest};
use crate::vm;

pub struct Executor;

impl Guest for Executor {
    fn execute(snapshot_path: String, code: String) -> Result<ExecutionResult, String> {
        let (mut bus, mut hart) = vm::load(vm::VmConfig {
            snapshot: snapshot_path.as_ref(),
            disk: None,
            capture_tx: false,
        })?;
        let stdout = run_code(&mut bus, &mut hart, &code);

        Ok(ExecutionResult {
            stdout,
            stderr: String::new(),
            exit_code: 0,
        })
    }

    fn session_start(
        snapshot_path: String,
        command: String,
        prompt: String,
    ) -> Result<u64, String> {
        SESSION_MANAGER.start_session(snapshot_path, command, prompt)
    }

    fn session_exec(handle: u64, code: String) -> Result<String, String> {
        SESSION_MANAGER.exec_code(handle, code)
    }

    fn session_close(handle: u64) {
        SESSION_MANAGER.close_session(handle);
    }
}

fn run_code(bus: &mut MachineBus, hart: &mut Hart, code: &str) -> String {
    for byte in code.bytes() {
        bus.uart.push_rx(byte);
    }

    if !code.ends_with('\n') {
        bus.uart.push_rx(b'\n');
    }

    for byte in b"VPOD_EOF\n" {
        bus.uart.push_rx(*byte);
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
                eprintln!("[executor] trap {:?} at pc={:#x}", cause, hart.regs.pc);
                break;
            }
            StepResult::Halt => break,
        }
    }

    String::from_utf8_lossy(&bus.console.take_tx_buffer()).into_owned()
}
