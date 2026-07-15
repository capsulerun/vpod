use base64::Engine;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::LazyLock;

use crate::exports::vpod::sandbox::executor::ExecutionResult;
use crate::repl;
use crate::vm;

use machine::machine_bus::MachineBus;
use riscv_core::Hart;

const PYRUNNER_SENTINEL: &str = "---VPOD_DONE---";

pub struct Session {
    pub bus: MachineBus,
    pub hart: Hart,
    pub prompt: Vec<u8>,
    pub is_shell: bool,
    pub is_pyrunner: bool,
    pub has_pyrunner: bool,
    pub pyrunner_dirty: bool,
}

struct CachedBase {
    path: String,
    base: machine::cow_ram::CowRam,
    tail: Vec<u8>,
    flags: u8,
}

pub struct SessionManager {
    sessions: RefCell<HashMap<u64, Session>>,
    next_id: Cell<u64>,
    base_cache: RefCell<Option<CachedBase>>,
}

unsafe impl Sync for SessionManager {}

pub static SESSION_MANAGER: LazyLock<SessionManager> = LazyLock::new(|| SessionManager {
    sessions: RefCell::new(HashMap::new()),
    next_id: Cell::new(1),
    base_cache: RefCell::new(None),
});

impl SessionManager {
    fn ensure_base(&self, snapshot_path: &str) -> Result<(), String> {
        let cache = self.base_cache.borrow();
        let hit = matches!(&*cache, Some(c) if c.path == snapshot_path);
        drop(cache);

        if !hit {
            let (base, tail, flags) = vm::_read_base_and_tail(snapshot_path.as_ref())?;
            *self.base_cache.borrow_mut() = Some(CachedBase {
                path: snapshot_path.to_string(),
                base,
                tail,
                flags,
            });
        }
        Ok(())
    }

    pub fn start_session(
        &self,
        snapshot_path: String,
        command: String,
        prompt: String,
        mount_args: Vec<vm::MountArg>,
    ) -> Result<u64, String> {
        let ram_size = vm::ram_size_from_filename(std::path::Path::new(&snapshot_path))
            .unwrap_or(256 * 1024 * 1024);

        self.ensure_base(&snapshot_path)?;

        let cache = self.base_cache.borrow();
        let cached = cache.as_ref().unwrap();
        let flags = cached.flags;
        let (mut bus, mut hart) = vm::_bus_from_base(&cached.base, ram_size, &mount_args, true);

        machine::snapshot::restore_devices(
            &mut bus,
            &mut hart,
            &mut std::io::Cursor::new(&cached.tail),
        )
        .map_err(|e| format!("failed to restore devices: {e}"))?;
        drop(cache);

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
            if !shell_ready {
                repl::wait_for_prompt(&mut bus, &mut hart, &prompt_bytes);
                bus.uart.drain_tx();
            }

            repl::shell_init(&mut bus, &mut hart, &prompt_bytes);
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

        if !mount_args.is_empty() && (is_shell || use_pyrunner) {
            let mut script = String::new();
            for (i, m) in mount_args.iter().enumerate() {
                script.push_str(&format!(
                    "mkdir -p {0} && mount -t virtiofs vfs{1} {0} 2>/dev/null; ",
                    m.guest_path, i
                ));
            }

            script.push('\n');
            for byte in script.bytes() {
                bus.uart.push_rx(byte);
            }

            repl::wait_for_prompt(&mut bus, &mut hart, &prompt_bytes);
            bus.uart.drain_tx();
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
                has_pyrunner: python_ready,
                pyrunner_dirty: false,
            },
        );

        Ok(id)
    }

    pub fn exec_code(
        &self,
        handle: u64,
        code: String,
        timeout: Option<u64>,
    ) -> Result<ExecutionResult, String> {
        let mut sessions = self.sessions.borrow_mut();
        let session = sessions
            .get_mut(&handle)
            .ok_or_else(|| format!("invalid session handle: {handle}"))?;

        session.bus.uart.drain_tx();
        session.bus.uart_stderr.drain_tx();
        session.bus.uart_ctrl.drain_tx();
        session.bus.uart_data.drain_tx();

        let use_pyrunner = if code.starts_with('\0') {
            session.has_pyrunner
        } else {
            session.is_pyrunner
        };

        let code = if let Some(s) = code.strip_prefix('\0') {
            s.to_string()
        } else {
            code
        };

        if use_pyrunner {
            if session.pyrunner_dirty {
                restart_pyrunner(session);
                session.pyrunner_dirty = false;
            }

            let b64 = base64::engine::general_purpose::STANDARD.encode(code.as_bytes());
            for byte in b64.bytes() {
                session.bus.uart_data.push_rx(byte);
            }
            session.bus.uart_data.push_rx(b'\n');

            let stdout = repl::capture_output(
                &mut session.bus,
                &mut session.hart,
                b"",
                timeout.unwrap_or(120),
                false,
                Some(PYRUNNER_SENTINEL),
                true,
            );

            let stderr_bytes = session.bus.uart_stderr.drain_tx();
            let stderr = String::from_utf8_lossy(&stderr_bytes)
                .trim_end()
                .to_string();

            let ctrl_bytes = repl::drain_ctrl_with_grace(&mut session.bus, &mut session.hart);
            let exit_code = match ctrl_bytes.first() {
                Some(byte) => *byte as u32,
                None => {
                    session.pyrunner_dirty = true;
                    124
                }
            };

            Ok(ExecutionResult {
                stdout,
                stderr,
                exit_code,
            })
        } else {
            let cmd = if session.is_shell {
                format!("{{ {code}; }} 2>/dev/ttyS1\n")
            } else {
                format!("{code}\n")
            };

            for byte in cmd.bytes() {
                session.bus.uart.push_rx(byte);
            }

            let stdout = repl::capture_output(
                &mut session.bus,
                &mut session.hart,
                &session.prompt,
                timeout.unwrap_or(30),
                session.is_shell,
                None,
                false,
            );

            let stderr_bytes = session.bus.uart_stderr.drain_tx();
            let stderr = String::from_utf8_lossy(&stderr_bytes)
                .trim_end()
                .to_string();

            let mut timed_out = false;
            let exit_code = if session.is_shell {
                let ctrl_bytes = repl::drain_ctrl_with_grace(&mut session.bus, &mut session.hart);
                match ctrl_bytes.first() {
                    Some(byte) => *byte as u32,
                    None => {
                        timed_out = true;
                        124
                    }
                }
            } else {
                0
            };

            if session.is_shell && timed_out {
                session.bus.uart.push_rx(0x03);
                repl::wait_for_prompt(&mut session.bus, &mut session.hart, &session.prompt);
                session.bus.uart.drain_tx();
                session.bus.uart_stderr.drain_tx();
                session.bus.uart_ctrl.drain_tx();
            }

            Ok(ExecutionResult {
                stdout,
                stderr,
                exit_code,
            })
        }
    }

    pub fn close_session(&self, handle: u64) {
        self.sessions.borrow_mut().remove(&handle);
    }

    pub fn suspend_session(&self, handle: u64) -> Result<Vec<u8>, String> {
        let mut sessions = self.sessions.borrow_mut();
        let session = sessions
            .get_mut(&handle)
            .ok_or_else(|| format!("invalid session handle: {handle}"))?;

        if session.pyrunner_dirty {
            restart_pyrunner(session);
            session.pyrunner_dirty = false;
        }

        let mut buf = Vec::new();
        machine::snapshot::save_delta(&session.bus, &session.hart, &mut buf)
            .map_err(|e| format!("suspend failed: {e}"))?;

        let meta = format!(
            "{}|{}|{}",
            if session.is_shell {
                "shell"
            } else if session.is_pyrunner {
                "pyrunner"
            } else {
                "custom"
            },
            session.has_pyrunner,
            String::from_utf8_lossy(&session.prompt),
        );

        let meta_bytes = meta.as_bytes();
        buf.extend_from_slice(meta_bytes);
        buf.extend_from_slice(&(meta_bytes.len() as u32).to_le_bytes());

        Ok(buf)
    }

    pub fn resume_session(
        &self,
        snapshot_path: String,
        delta: Vec<u8>,
        _command: String,
        _prompt: String,
        mount_args: Vec<vm::MountArg>,
    ) -> Result<u64, String> {
        let ram_size = vm::ram_size_from_filename(std::path::Path::new(&snapshot_path))
            .unwrap_or(256 * 1024 * 1024);

        self.ensure_base(&snapshot_path)?;

        let cache = self.base_cache.borrow();
        let cached = cache.as_ref().unwrap();
        let (mut bus, mut hart) = vm::_bus_from_base(&cached.base, ram_size, &mount_args, true);
        drop(cache);

        let meta_len_offset = delta.len() - 4;
        let meta_len = u32::from_le_bytes(delta[meta_len_offset..].try_into().unwrap()) as usize;
        let meta_offset = meta_len_offset - meta_len;
        let meta_str = String::from_utf8_lossy(&delta[meta_offset..meta_len_offset]).to_string();
        let delta_bytes = &delta[..meta_offset];

        let mut cursor = std::io::Cursor::new(delta_bytes);
        machine::snapshot::restore_delta(&mut bus, &mut hart, &mut cursor)
            .map_err(|e| format!("resume failed: {e}"))?;

        let parts: Vec<&str> = meta_str.splitn(3, '|').collect();
        let prompt_bytes: Vec<u8> = if parts.len() == 3 {
            parts[2].as_bytes().to_vec()
        } else {
            b"# ".to_vec()
        };

        bus.uart.drain_tx();
        bus.uart_stderr.drain_tx();
        bus.uart_ctrl.drain_tx();
        hart.is_waiting = false;
        repl::sync_clock(&mut bus, &mut hart, &prompt_bytes);
        let (is_shell, is_pyrunner, has_pyrunner, prompt) = if parts.len() == 3 {
            let kind = parts[0];
            let has_py = parts[1] == "true";
            let prompt = parts[2].as_bytes().to_vec();
            (kind == "shell", kind == "pyrunner", has_py, prompt)
        } else {
            (true, false, false, b"# ".to_vec())
        };

        let id = self.next_id.get();
        self.next_id.set(id + 1);
        self.sessions.borrow_mut().insert(
            id,
            Session {
                bus,
                hart,
                prompt,
                is_shell,
                is_pyrunner,
                has_pyrunner,
                pyrunner_dirty: false,
            },
        );

        Ok(id)
    }
}

fn restart_pyrunner(session: &mut Session) {
    let restart = b"pkill -9 -f pyrunner.py; python3 /usr/lib/vpod/pyrunner.py &\n";
    for byte in restart {
        session.bus.uart.push_rx(*byte);
    }

    repl::wait_for_prompt(&mut session.bus, &mut session.hart, &session.prompt);

    session.bus.uart.drain_tx();
    session.bus.uart_stderr.drain_tx();
    session.bus.uart_ctrl.drain_tx();
    session.bus.uart_data.drain_tx();

    repl::settle(&mut session.bus, &mut session.hart, 2_000_000_000);
    session.bus.uart_data.drain_tx();

    let probe = base64::engine::general_purpose::STANDARD.encode(b"pass");
    for byte in probe.bytes() {
        session.bus.uart_data.push_rx(byte);
    }
    session.bus.uart_data.push_rx(b'\n');

    let _ = repl::capture_output(
        &mut session.bus,
        &mut session.hart,
        b"",
        10,
        false,
        Some(PYRUNNER_SENTINEL),
        true,
    );

    session.bus.uart_data.drain_tx();
    session.bus.uart_stderr.drain_tx();
    session.bus.uart_ctrl.drain_tx();
}
