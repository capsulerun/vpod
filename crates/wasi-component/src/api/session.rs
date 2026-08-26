use base64::Engine;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::LazyLock;

use crate::exports::vpod::sandbox::executor::{ExecMode, ExecutionResult, SliceOutput};
use crate::repl;
use crate::vm;

use machine::machine_bus::MachineBus;
use riscv_core::Hart;

const PYRUNNER_SENTINEL: &str = "---VPOD_DONE---";

const MAX_INLINE_EXEC: usize = 1900;
const STAGE_CHUNK: usize = 1500;
const STAGE_PATH: &str = "/tmp/.vpod_cmd";
const STDIN_PATH: &str = "/tmp/.vpod_stdin";

const STDIN_STAGE_CHUNK: usize = STAGE_CHUNK;

const PYRUNNER_MAX_LINE: usize = 3800;
const PYRUNNER_STAGE_CHUNK: usize = 2500;

const SHELL_PROMPT_SENTINEL: &[u8] = b"\x1fvpod\x1f";

const AOT_MISMATCH_PROBE_THRESHOLD: u64 = 64;

fn warn_if_aot_mismatch(hart: &Hart) {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    if !hart.blocks.aot_enabled() {
        return;
    }
    let (probes, matches) = hart.blocks.aot_match_stats();
    if probes >= AOT_MISMATCH_PROBE_THRESHOLD
        && matches == 0
        && !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        eprintln!(
            "[vpod] warning: the bundled AOT module does not match this snapshot; \
             running at interpreter speed. Upgrade the vpod package or re-pull \
             the snapshot so the two agree."
        );
    }
}

pub struct Session {
    pub bus: MachineBus,
    pub hart: Hart,
    pub prompt: Vec<u8>,
    pub is_shell: bool,
    pub is_pyrunner: bool,
    pub has_pyrunner: bool,
    pub pyrunner_dirty: bool,
    pub pyrunner_reseeded: bool,
    pub shell_lost: bool,
    pub exec: Option<repl::ExecState>,
    /// Input written before the command started. Staged to a file rather than
    /// pushed at the terminal, so EOF belongs to the command instead of leaking
    /// to the shell behind it.
    pub staged_stdin: Vec<u8>,
}

fn recover_shell(session: &mut Session) {
    session.bus.uart.push_rx(0x03);
    let mut recovered = repl::wait_for_prompt(&mut session.bus, &mut session.hart, &session.prompt);

    if !recovered {
        session.bus.uart.push_rx(0x04);
        recovered = repl::wait_for_prompt(&mut session.bus, &mut session.hart, &session.prompt);
    }

    if recovered {
        repl::absorb_stray_prompt(&mut session.bus, &mut session.hart, &session.prompt);
    }

    session.bus.uart.drain_tx();
    session.bus.uart_stderr.drain_tx();
    session.bus.uart_ctrl.drain_tx();

    session.shell_lost = !recovered;
}

const SHELL_LOST_MESSAGE: &str = "vpod: the shell did not come back from a timed-out command. Something that \
     ignores Ctrl-C was left in the foreground, an interactive python3 or a \
     pager for instance. This sandbox cannot run further commands, so create a \
     new one; `code.run` is unaffected.";

fn begin_shell_exec(session: &mut Session, code: String, timeout_secs: u64, mode: ExecMode) {
    if session.exec.take().is_some() {
        recover_shell(session);
    }

    let code = if session.is_shell && code.len() > MAX_INLINE_EXEC {
        stage_long_command(session, &code)
    } else {
        code
    };

    let pending_stdin = std::mem::take(&mut session.staged_stdin);
    let staged = !pending_stdin.is_empty() && session.is_shell && mode == ExecMode::Piped;
    if staged {
        stage_stdin(session, &pending_stdin);
    }

    let cmd = if session.is_shell {
        match mode {
            ExecMode::Closed => format!("{{\n{code}\n}} </dev/null 2>/dev/ttyS1\n"),
            ExecMode::Piped if staged => format!(
                "{{\n\
                 rm -f {STDIN_PATH}\n\
                 {code}\n\
                 }} < {STDIN_PATH} 2>/dev/ttyS1\n"
            ),
            ExecMode::Piped => format!("{{\n{code}\n}} 2>/dev/ttyS1\n"),
            ExecMode::Terminal => format!(
                "{{\n\
                 stty -icanon -echo\n\
                 {code}\n\
                 }} 2>&1\n"
            ),
        }
    } else {
        format!("{code}\n")
    };

    for byte in cmd.bytes() {
        session.bus.uart.push_rx(byte);
    }

    if mode == ExecMode::Terminal {
        for byte in &pending_stdin {
            session.bus.uart.push_rx(*byte);
        }
    }

    session.exec = Some(repl::ExecState::with_mode(
        timeout_secs,
        mode == ExecMode::Terminal,
    ));
}

fn restore_terminal(session: &mut Session) {
    for byte in b"stty icanon -echo\n" {
        session.bus.uart.push_rx(*byte);
    }

    let prompt = session.prompt.clone();
    repl::wait_for_prompt(&mut session.bus, &mut session.hart, &prompt);

    session.bus.uart.drain_tx();
    session.bus.uart_stderr.drain_tx();
    session.bus.uart_ctrl.drain_tx();
}

fn run_shell_slice(
    session: &mut Session,
    state: &mut repl::ExecState,
    slice_nanos: u64,
) -> repl::SliceOutcome {
    repl::run_slice(
        &mut session.bus,
        &mut session.hart,
        &session.prompt,
        session.is_shell,
        None,
        false,
        state,
        slice_nanos,
    )
}

fn finish_shell_exec(session: &mut Session, state: repl::ExecState) -> ExecutionResult {
    let was_terminal = state.is_terminal();
    let stderr_tail = repl::finish_stderr(&state);
    let stdout = repl::finish_output(&session.bus, None, false, state);

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

    let mut stderr = stderr_tail;
    stderr.push_str(&String::from_utf8_lossy(
        &session.bus.uart_stderr.drain_tx(),
    ));
    let stderr = stderr.trim_end().to_string();

    if session.is_shell && timed_out {
        recover_shell(session);
    }

    if session.is_shell && was_terminal && !session.shell_lost {
        restore_terminal(session);
    }

    ExecutionResult {
        stdout,
        stderr,
        exit_code,
    }
}

fn stage_long_command(session: &mut Session, code: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(code.as_bytes());
    let prompt = session.prompt.clone();

    for (i, chunk) in encoded.as_bytes().chunks(STAGE_CHUNK).enumerate() {
        let chunk = std::str::from_utf8(chunk).unwrap_or_default();
        let redirect = if i == 0 { ">" } else { ">>" };
        let upload = format!("printf %s {chunk} {redirect} {STAGE_PATH}.b64\n");

        for byte in upload.bytes() {
            session.bus.uart.push_rx(byte);
        }
        repl::wait_for_prompt(&mut session.bus, &mut session.hart, &prompt);
        session.bus.uart.drain_tx();
        session.bus.uart_stderr.drain_tx();
        session.bus.uart_ctrl.drain_tx();
    }

    format!(
        "base64 -d {STAGE_PATH}.b64 > {STAGE_PATH}.sh && rm -f {STAGE_PATH}.b64; \
         sh {STAGE_PATH}.sh"
    )
}

fn stage_stdin(session: &mut Session, data: &[u8]) {
    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
    let prompt = session.prompt.clone();

    for (i, chunk) in encoded.as_bytes().chunks(STDIN_STAGE_CHUNK).enumerate() {
        let chunk = std::str::from_utf8(chunk).unwrap_or_default();
        let redirect = if i == 0 { ">" } else { ">>" };
        let upload = format!("printf %s {chunk} {redirect} {STDIN_PATH}.b64\n");

        for byte in upload.bytes() {
            session.bus.uart.push_rx(byte);
        }
        repl::wait_for_prompt(&mut session.bus, &mut session.hart, &prompt);
        session.bus.uart.drain_tx();
        session.bus.uart_stderr.drain_tx();
        session.bus.uart_ctrl.drain_tx();
    }

    let decode = format!("base64 -d {STDIN_PATH}.b64 > {STDIN_PATH} && rm -f {STDIN_PATH}.b64\n");
    for byte in decode.bytes() {
        session.bus.uart.push_rx(byte);
    }
    repl::wait_for_prompt(&mut session.bus, &mut session.hart, &prompt);
    session.bus.uart.drain_tx();
    session.bus.uart_stderr.drain_tx();
    session.bus.uart_ctrl.drain_tx();
}

fn stage_pyrunner_code(session: &mut Session, encoded: &str) -> String {
    for (i, chunk) in encoded.as_bytes().chunks(PYRUNNER_STAGE_CHUNK).enumerate() {
        let chunk = std::str::from_utf8(chunk).unwrap_or_default();
        let mode = if i == 0 { "w" } else { "a" };
        let statement = format!("open('{STAGE_PATH}.py.b64','{mode}').write('{chunk}')");
        let line = base64::engine::general_purpose::STANDARD.encode(statement.as_bytes());

        for byte in line.bytes() {
            session.bus.uart_data.push_rx(byte);
        }
        session.bus.uart_data.push_rx(b'\n');

        repl::capture_output(
            &mut session.bus,
            &mut session.hart,
            b"",
            30,
            false,
            Some(PYRUNNER_SENTINEL),
            true,
        );
        session.bus.uart_ctrl.drain_tx();
        session.bus.uart_data.drain_tx();
    }

    let loader = format!(
        "import base64 as _vb, os as _vo\n\
         _vs = _vb.b64decode(open('{STAGE_PATH}.py.b64').read())\n\
         _vo.remove('{STAGE_PATH}.py.b64')\n\
         exec(compile(_vs, '<vpod>', 'exec'))"
    );
    base64::engine::general_purpose::STANDARD.encode(loader.as_bytes())
}

fn install_prompt_sentinel(bus: &mut MachineBus, hart: &mut Hart, prompt_bytes: &mut Vec<u8>) {
    if prompt_bytes == SHELL_PROMPT_SENTINEL {
        return;
    }

    let export = "set -o ignoreeof; export PS2=''; export PS1='$(__ec $?)'\"$(printf '\\037vpod\\037')\"\n";
    for byte in export.bytes() {
        bus.uart.push_rx(byte);
    }

    repl::wait_for_prompt(bus, hart, SHELL_PROMPT_SENTINEL);
    bus.uart.drain_tx();
    bus.uart_stderr.drain_tx();
    bus.uart_ctrl.drain_tx();
    *prompt_bytes = SHELL_PROMPT_SENTINEL.to_vec();
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

        let mut prompt_bytes = if use_pyrunner {
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
                repl::sync_clock_and_reseed(&mut bus, &mut hart, &prompt_bytes);
            }
        } else if use_pyrunner {
            if !shell_ready {
                repl::wait_for_prompt(&mut bus, &mut hart, &prompt_bytes);
                bus.uart.drain_tx();
            }

            repl::shell_init(&mut bus, &mut hart, &prompt_bytes);
        } else {
            repl::reseed(&mut bus, &mut hart, &prompt_bytes);

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

        if is_shell {
            install_prompt_sentinel(&mut bus, &mut hart, &mut prompt_bytes);
        }

        warn_if_aot_mismatch(&hart);

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
                pyrunner_reseeded: false,
                shell_lost: false,
                exec: None,
                staged_stdin: Vec::new(),
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

        if session.shell_lost {
            return Err(SHELL_LOST_MESSAGE.to_string());
        }

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

            if !session.pyrunner_reseeded {
                reseed_pyrunner(session);
                session.pyrunner_reseeded = true;
            }

            let b64 = base64::engine::general_purpose::STANDARD.encode(code.as_bytes());
            let b64 = if b64.len() > PYRUNNER_MAX_LINE {
                stage_pyrunner_code(session, &b64)
            } else {
                b64
            };

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

            let ctrl_bytes = repl::drain_ctrl_with_grace(&mut session.bus, &mut session.hart);
            let exit_code = match ctrl_bytes.first() {
                Some(byte) => *byte as u32,
                None => {
                    session.pyrunner_dirty = true;
                    124
                }
            };

            let stderr_bytes = session.bus.uart_stderr.drain_tx();
            let stderr = String::from_utf8_lossy(&stderr_bytes)
                .trim_end()
                .to_string();

            Ok(ExecutionResult {
                stdout,
                stderr,
                exit_code,
            })
        } else {
            begin_shell_exec(session, code, timeout.unwrap_or(30), ExecMode::Closed);

            let mut state = session
                .exec
                .take()
                .unwrap_or_else(|| repl::ExecState::new(30));
            while run_shell_slice(session, &mut state, u64::MAX) == repl::SliceOutcome::Yielded {}

            Ok(finish_shell_exec(session, state))
        }
    }

    pub fn exec_slice(
        &self,
        handle: u64,
        code: Option<String>,
        timeout: Option<u64>,
        slice_nanos: u64,
        mode: ExecMode,
    ) -> Result<SliceOutput, String> {
        let mut sessions = self.sessions.borrow_mut();
        let session = sessions
            .get_mut(&handle)
            .ok_or_else(|| format!("invalid session handle: {handle}"))?;

        if session.shell_lost {
            return Err(SHELL_LOST_MESSAGE.to_string());
        }

        if let Some(code) = code {
            if session.is_pyrunner || code.starts_with('\0') {
                return Err("slicing is not supported for code.run()".to_string());
            }

            session.bus.uart.drain_tx();
            session.bus.uart_stderr.drain_tx();
            session.bus.uart_ctrl.drain_tx();
            session.bus.uart_data.drain_tx();

            begin_shell_exec(session, code, timeout.unwrap_or(30), mode);
        }

        let mut state = match session.exec.take() {
            Some(state) => state,
            None => return Err("no command is running in this session".to_string()),
        };

        let outcome = run_shell_slice(session, &mut state, slice_nanos);
        state.absorb_stderr(&session.bus.uart_stderr.drain_tx());

        if outcome == repl::SliceOutcome::Yielded {
            let prompt = session.prompt.clone();
            let stdout = repl::drain_output(&mut state, &prompt);
            let stderr = repl::drain_stderr(&mut state);
            session.exec = Some(state);

            return Ok(SliceOutput {
                stdout,
                stderr,
                exit_code: None,
            });
        }

        let finished = finish_shell_exec(session, state);

        Ok(SliceOutput {
            stdout: finished.stdout,
            stderr: finished.stderr,
            exit_code: Some(finished.exit_code),
        })
    }

    pub fn write_stdin(&self, handle: u64, data: Vec<u8>) -> Result<(), String> {
        let mut sessions = self.sessions.borrow_mut();
        let session = sessions
            .get_mut(&handle)
            .ok_or_else(|| format!("invalid session handle: {handle}"))?;

        if session.exec.is_none() {
            session.staged_stdin.extend_from_slice(&data);
            return Ok(());
        }

        for byte in data {
            session.bus.uart.push_rx(byte);
        }

        Ok(())
    }

    pub fn interrupt_session(&self, handle: u64) -> Result<(), String> {
        let mut sessions = self.sessions.borrow_mut();
        let session = sessions
            .get_mut(&handle)
            .ok_or_else(|| format!("invalid session handle: {handle}"))?;

        if session.exec.is_none() {
            return Ok(());
        }

        session.bus.uart.push_rx(0x03);
        Ok(())
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
            "{}|{}|{}|{}",
            if session.is_shell {
                "shell"
            } else if session.is_pyrunner {
                "pyrunner"
            } else {
                "custom"
            },
            session.has_pyrunner,
            session.pyrunner_reseeded,
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

        let parts: Vec<&str> = meta_str.splitn(4, '|').collect();
        let prompt_bytes: Vec<u8> = if parts.len() == 4 {
            parts[3].as_bytes().to_vec()
        } else {
            b"# ".to_vec()
        };

        bus.uart.drain_tx();
        bus.uart_stderr.drain_tx();
        bus.uart_ctrl.drain_tx();
        hart.is_waiting = false;
        repl::sync_clock(&mut bus, &mut hart, &prompt_bytes);
        let (is_shell, is_pyrunner, has_pyrunner, pyrunner_reseeded, mut prompt) =
            if parts.len() == 4 {
                let kind = parts[0];
                let has_py = parts[1] == "true";
                let reseeded = parts[2] == "true";
                let prompt = parts[3].as_bytes().to_vec();
                (
                    kind == "shell",
                    kind == "pyrunner",
                    has_py,
                    reseeded,
                    prompt,
                )
            } else {
                (true, false, false, false, b"# ".to_vec())
            };

        if is_shell {
            install_prompt_sentinel(&mut bus, &mut hart, &mut prompt);
        }

        warn_if_aot_mismatch(&hart);

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
                pyrunner_reseeded,
                shell_lost: false,
                exec: None,
                staged_stdin: Vec::new(),
            },
        );

        Ok(id)
    }
}

const PYRUNNER_RESEED_CODE: &str = "\
import sys, random
random.seed()
numpy = sys.modules.get('numpy')
if numpy is not None:
    numpy.random.seed()
del sys, random, numpy";

fn warn_if_pyrunner_unseeded(complaint: &[u8]) {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    if complaint.is_empty() || WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }

    eprintln!(
        "[vpod] warning: could not reseed the interpreter's random module, so every \
         sandbox from this snapshot will get the same random.random() sequence. \
         os.urandom, secrets and uuid4 are unaffected. ({})",
        String::from_utf8_lossy(complaint).trim()
    );
}

fn reseed_pyrunner(session: &mut Session) {
    let line = base64::engine::general_purpose::STANDARD.encode(PYRUNNER_RESEED_CODE.as_bytes());

    for byte in line.bytes() {
        session.bus.uart_data.push_rx(byte);
    }
    session.bus.uart_data.push_rx(b'\n');

    let _ = repl::capture_output(
        &mut session.bus,
        &mut session.hart,
        b"",
        30,
        false,
        Some(PYRUNNER_SENTINEL),
        true,
    );

    repl::drain_ctrl_with_grace(&mut session.bus, &mut session.hart);
    warn_if_pyrunner_unseeded(&session.bus.uart_stderr.drain_tx());

    session.bus.uart_data.drain_tx();
}

fn restart_pyrunner(session: &mut Session) {
    let restart = b"pkill -9 -f pyrunner.py; PYR=/usr/bin/python3.real; [ -x $PYR ] || PYR=python3; $PYR /usr/lib/vpod/pyrunner.py &\n";
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

    session.pyrunner_reseeded = true;
}
