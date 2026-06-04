use crate::api::session::SESSION_MANAGER;
use crate::exports::vpod::sandbox::executor::{ExecutionResult, Guest};
use crate::repl;
use crate::vm;

const DEFAULT_PROMPT: &[u8] = b"# ";

pub struct Executor;

impl Guest for Executor {
    fn execute(snapshot_path: String, code: String) -> Result<ExecutionResult, String> {
        let (mut bus, mut hart) = vm::load(vm::VmConfig {
            snapshot: snapshot_path.as_ref(),
            disk: None,
            capture_tx: true,
        })?;

        for byte in b"/bin/sh\n" {
            bus.uart.push_rx(*byte);
        }

        repl::wait_for_prompt(&mut bus, &mut hart, DEFAULT_PROMPT);
        bus.uart.drain_tx();

        let wrapped = format!("{code}; echo \"__EXIT:$?\"");
        for byte in wrapped.bytes() {
            bus.uart.push_rx(byte);
        }
        bus.uart.push_rx(b'\n');

        let raw_output = repl::capture_output_until_prompt(&mut bus, &mut hart, DEFAULT_PROMPT);

        let (stdout, exit_code) = parse_exit_code(&raw_output);

        Ok(ExecutionResult {
            stdout,
            stderr: String::new(),
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

    fn session_exec(handle: u64, code: String) -> Result<String, String> {
        SESSION_MANAGER.exec_code(handle, code)
    }

    fn session_close(handle: u64) {
        SESSION_MANAGER.close_session(handle);
    }
}

fn parse_exit_code(output: &str) -> (String, u32) {
    let marker = "__EXIT:";

    if let Some(pos) = output.rfind(marker) {
        let code_str = output[pos + marker.len()..].trim();
        let exit_code = code_str.parse::<u32>().unwrap_or(0);
        let stdout = output[..pos].trim_end().to_string();
        (stdout, exit_code)
    } else {
        (output.to_string(), 0)
    }
}
