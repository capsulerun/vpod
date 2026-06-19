use base64::Engine;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::LazyLock;

use crate::exports::vpod::sandbox::executor::ExecutionResult;
use crate::repl;
use machine::machine_bus::MachineBus;
use riscv_core::Hart;

pub struct Session {
    pub bus: MachineBus,
    pub hart: Hart,
    pub prompt: Vec<u8>,
    pub is_shell: bool,
    pub is_pyrunner: bool,
}

pub struct SessionManager {
    sessions: RefCell<HashMap<u64, Session>>,
    next_id: Cell<u64>,
}

unsafe impl Sync for SessionManager {}

pub static SESSION_MANAGER: LazyLock<SessionManager> = LazyLock::new(|| SessionManager {
    sessions: RefCell::new(HashMap::new()),
    next_id: Cell::new(1),
});

impl SessionManager {
    pub fn start_session(
        &self,
        snapshot_path: String,
        command: String,
        prompt: String,
    ) -> Result<u64, String> {
        let (mut bus, mut hart, flags) = crate::vm::load(crate::vm::VmConfig {
            snapshot: snapshot_path.as_ref(),
            disk: None,
            capture_tx: true,
        })?;

        let is_shell = command == "/bin/sh" || command == "sh" || command == "/bin/ash";
        let is_python = command == "python3" || command == "/usr/bin/python3";
        let shell_ready = flags & machine::snapshot::FLAG_SHELL_READY != 0;
        let python_ready = flags & machine::snapshot::FLAG_PYTHON_READY != 0;
        let use_pyrunner = is_python && python_ready;

        let prompt_bytes = if use_pyrunner {
            b"# ".to_vec()
        } else {
            prompt.into_bytes()
        };

        if is_shell {
            if !shell_ready {
                for byte in command.bytes() {
                    bus.uart.push_rx(byte);
                }

                bus.uart.push_rx(b'\n');
                repl::wait_for_prompt(&mut bus, &mut hart, &prompt_bytes);

                bus.uart.drain_tx();
                repl::shell_init(&mut bus, &mut hart, &prompt_bytes);
            } else {
                repl::sync_clock(&mut bus, &mut hart, &prompt_bytes);
            }
        } else if use_pyrunner {
            repl::sync_clock(&mut bus, &mut hart, &prompt_bytes);
        } else {
            let launch = format!("stty -echo; {command}\n");
            for byte in launch.bytes() {
                bus.uart.push_rx(byte);
            }

            repl::wait_for_prompt(&mut bus, &mut hart, &prompt_bytes);

            bus.uart.drain_tx();
            bus.uart_stderr.drain_tx();
            bus.uart_ctrl.drain_tx();
        }

        let id = self.next_id.get();
        self.next_id.set(id + 1);
        self.sessions.borrow_mut().insert(
            id,
            Session {
                bus,
                hart,
                prompt: prompt_bytes,
                is_shell,
                is_pyrunner: use_pyrunner,
            },
        );

        Ok(id)
    }

    pub fn exec_code(&self, handle: u64, code: String) -> Result<ExecutionResult, String> {
        let mut sessions = self.sessions.borrow_mut();
        let session = sessions
            .get_mut(&handle)
            .ok_or_else(|| format!("invalid session handle: {handle}"))?;

        session.bus.uart_stderr.drain_tx();
        session.bus.uart_ctrl.drain_tx();

        const PYRUNNER_SENTINEL: &str = "---VPOD_DONE---";

        let cmd = if session.is_pyrunner {
            let b64 = base64::engine::general_purpose::STANDARD.encode(code.as_bytes());
            format!("echo {} >&9; cat /tmp/py.resp 2>/dev/ttyS1\n", b64)
        } else if session.is_shell {
            format!("{{ {code}; }} 2>/dev/ttyS1\n")
        } else {
            format!("{code}\n")
        };

        for byte in cmd.bytes() {
            session.bus.uart.push_rx(byte);
        }

        let stdout = repl::capture_output_impl(
            &mut session.bus,
            &mut session.hart,
            &session.prompt,
            30,
            session.is_shell || session.is_pyrunner,
        );

        let stdout = if session.is_pyrunner {
            stdout
                .lines()
                .filter(|l| *l != PYRUNNER_SENTINEL)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            stdout
        };

        let stderr_bytes = session.bus.uart_stderr.drain_tx();
        let stderr = String::from_utf8_lossy(&stderr_bytes)
            .trim_end()
            .to_string();

        let exit_code = if session.is_shell || session.is_pyrunner {
            let ctrl_bytes = session.bus.uart_ctrl.drain_tx();
            ctrl_bytes.first().copied().unwrap_or(0) as u32
        } else {
            0
        };

        Ok(ExecutionResult {
            stdout,
            stderr,
            exit_code,
        })
    }

    pub fn close_session(&self, handle: u64) {
        self.sessions.borrow_mut().remove(&handle);
    }
}
