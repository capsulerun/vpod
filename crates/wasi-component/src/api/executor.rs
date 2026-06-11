use crate::api::session::SESSION_MANAGER;
use crate::exports::vpod::sandbox::executor::{ExecutionResult, Guest};
use crate::repl;
use crate::vm;

const DEFAULT_PROMPT: &[u8] = b"# ";

pub struct Executor;

impl Guest for Executor {
    fn execute(snapshot_path: String, code: String) -> Result<ExecutionResult, String> {
        let (mut bus, mut hart, flags) = vm::load(vm::VmConfig {
            snapshot: snapshot_path.as_ref(),
            disk: None,
            capture_tx: true,
        })?;

        let shell_ready = flags & machine::snapshot::FLAG_SHELL_READY != 0;
        if !shell_ready {
            for byte in b"setsid sh\n" {
                bus.uart.push_rx(*byte);
            }
            repl::wait_for_prompt(&mut bus, &mut hart, DEFAULT_PROMPT);
            bus.uart.drain_tx();

            repl::shell_init(&mut bus, &mut hart, DEFAULT_PROMPT);
        }

        let cmd = format!("{{ {code}; }} 2>/dev/ttyS1\n");
        for byte in cmd.bytes() {
            bus.uart.push_rx(byte);
        }

        let stdout = repl::capture_output_impl(&mut bus, &mut hart, DEFAULT_PROMPT, 30, true);

        let stderr_bytes = bus.uart_stderr.drain_tx();
        let stderr = String::from_utf8_lossy(&stderr_bytes)
            .trim_end()
            .to_string();

        let ctrl_bytes = bus.uart_ctrl.drain_tx();
        let exit_code = ctrl_bytes.first().copied().unwrap_or(0) as u32;

        Ok(ExecutionResult {
            stdout,
            stderr,
            exit_code,
        })
    }

    fn session_start(
        snapshot_path: String,
        command: String,
        prompt: String,
    ) -> Result<u64, String> {
        SESSION_MANAGER.start_session(snapshot_path, command, prompt)
    }

    fn session_exec(handle: u64, code: String) -> Result<ExecutionResult, String> {
        SESSION_MANAGER.exec_code(handle, code)
    }

    fn session_close(handle: u64) {
        SESSION_MANAGER.close_session(handle);
    }
}
