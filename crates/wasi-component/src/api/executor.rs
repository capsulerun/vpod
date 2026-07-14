use crate::api::session::SESSION_MANAGER;
use crate::exports::vpod::sandbox::executor::{ExecutionResult, Guest, MountEntry};
use crate::vm;

pub struct Executor;

impl Guest for Executor {
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

    fn session_exec(
        handle: u64,
        code: String,
        timeout: Option<u64>,
    ) -> Result<ExecutionResult, String> {
        SESSION_MANAGER.exec_code(handle, code, timeout)
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

        SESSION_MANAGER.resume_session(snapshot_path, delta, command, prompt, mount_args)
    }
}
