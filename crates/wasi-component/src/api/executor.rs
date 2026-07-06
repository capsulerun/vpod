use crate::api::session::SESSION_MANAGER;
use crate::exports::vpod::sandbox::executor::{ExecutionResult, Guest, MountEntry};
use crate::repl;
use crate::vm;

const DEFAULT_PROMPT: &[u8] = b"# ";

pub struct Executor;

impl Guest for Executor {
    fn execute(snapshot_path: String, code: String) -> Result<ExecutionResult, String> {
        let (mut bus, mut hart, flags) = vm::load(vm::VmConfig {
            snapshot: snapshot_path.as_ref(),
            disk: None,
            mounts: vec![],
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
        } else {
            repl::sync_clock(&mut bus, &mut hart, DEFAULT_PROMPT);
        }

        let cmd = format!("{{ {code}; }} 2>/dev/ttyS1\n");
        for byte in cmd.bytes() {
            bus.uart.push_rx(byte);
        }

        let stdout =
            repl::capture_output(&mut bus, &mut hart, DEFAULT_PROMPT, 30, true, None, false);

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
        mounts: Vec<MountEntry>,
    ) -> Result<u64, String> {
        let mount_args: Vec<vm::MountArg> = mounts
            .into_iter()
            .map(|m| vm::MountArg {
                alias: m.host_alias,
                guest_path: m.guest_path,
                writable: m.writable,
            })
            .collect();
        SESSION_MANAGER.start_session(snapshot_path, command, prompt, mount_args)
    }

    fn session_exec(handle: u64, code: String) -> Result<ExecutionResult, String> {
        SESSION_MANAGER.exec_code(handle, code)
    }

    fn session_close(handle: u64) {
        SESSION_MANAGER.close_session(handle);
    }

    fn session_suspend(handle: u64) -> Result<Vec<u8>, String> {
        SESSION_MANAGER.suspend_session(handle)
    }

    fn session_resume(
        snapshot_path: String,
        delta: Vec<u8>,
        command: String,
        prompt: String,
        mounts: Vec<MountEntry>,
    ) -> Result<u64, String> {
        let mount_args: Vec<vm::MountArg> = mounts
            .into_iter()
            .map(|m| vm::MountArg {
                alias: m.host_alias,
                guest_path: m.guest_path,
                writable: m.writable,
            })
            .collect();

        SESSION_MANAGER.resume_session(snapshot_path, delta, command, prompt, mount_args)
    }
}
