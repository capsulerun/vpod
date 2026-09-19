use crate::api::session::SESSION_MANAGER;
use crate::exports::vpod::sandbox::executor::{
    EnvVar, ExecMode, ExecutionResult, Guest, MountEntry, SliceOutput, TraceOptions,
};
use crate::vm;

fn env_pairs(env: Vec<EnvVar>) -> Result<Vec<(String, String)>, String> {
    env.into_iter()
        .map(|entry| {
            let valid = !entry.name.is_empty()
                && !entry.name.starts_with(|c: char| c.is_ascii_digit())
                && entry
                    .name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_');

            if !valid {
                return Err(format!(
                    "environment variable name {:?} is not a plain identifier",
                    entry.name
                ));
            }

            Ok((entry.name, entry.value))
        })
        .collect()
}

pub struct Executor;

impl Guest for Executor {
    fn session_start(
        snapshot_path: String,
        command: String,
        prompt: String,
        mounts: Vec<MountEntry>,
        env: Vec<EnvVar>,
    ) -> Result<u64, String> {
        let mount_args: Vec<vm::MountArg> = mounts
            .into_iter()
            .map(|m| vm::MountArg {
                alias: m.host_alias,
                guest_path: m.guest_path,
                writable: m.writable,
            })
            .collect();

        SESSION_MANAGER.start_session(snapshot_path, command, prompt, mount_args, env_pairs(env)?)
    }

    fn session_exec(
        handle: u64,
        code: String,
        timeout: Option<u64>,
    ) -> Result<ExecutionResult, String> {
        SESSION_MANAGER.exec_code(handle, code, timeout)
    }

    fn session_exec_slice(
        handle: u64,
        code: Option<String>,
        timeout: Option<u64>,
        slice_nanos: u64,
        mode: ExecMode,
    ) -> Result<SliceOutput, String> {
        SESSION_MANAGER.exec_slice(handle, code, timeout, slice_nanos, mode)
    }

    fn session_interrupt(handle: u64) -> Result<(), String> {
        SESSION_MANAGER.interrupt_session(handle)
    }

    fn session_stdin(handle: u64, data: Vec<u8>) -> Result<(), String> {
        SESSION_MANAGER.write_stdin(handle, data)
    }

    fn session_close(handle: u64) {
        SESSION_MANAGER.close_session(handle);
    }

    fn session_suspend(handle: u64, delta_path: String) -> Result<u64, String> {
        let delta = SESSION_MANAGER.suspend_session(handle)?;
        std::fs::write(&delta_path, &delta)
            .map_err(|e| format!("failed to write delta to {delta_path}: {e}"))?;

        Ok(delta.len() as u64)
    }

    fn session_resume(
        snapshot_path: String,
        delta_path: String,
        command: String,
        prompt: String,
        mounts: Vec<MountEntry>,
        env: Vec<EnvVar>,
    ) -> Result<u64, String> {
        let delta = std::fs::read(&delta_path)
            .map_err(|e| format!("failed to read delta from {delta_path}: {e}"))?;

        let mount_args: Vec<vm::MountArg> = mounts
            .into_iter()
            .map(|m| vm::MountArg {
                alias: m.host_alias,
                guest_path: m.guest_path,
                writable: m.writable,
            })
            .collect();

        SESSION_MANAGER.resume_session(snapshot_path, delta, command, prompt, mount_args, env_pairs(env)?)
    }

    fn session_trace_start(handle: u64, options: TraceOptions) -> Result<(), String> {
        SESSION_MANAGER.trace_start(handle, options)
    }

    fn session_trace_drain(handle: u64, max_bytes: u32) -> Result<String, String> {
        SESSION_MANAGER.trace_drain(handle, max_bytes)
    }

    fn session_trace_stop(handle: u64) -> Result<(), String> {
        SESSION_MANAGER.trace_stop(handle)
    }
}
