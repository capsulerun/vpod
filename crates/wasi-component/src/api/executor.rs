use crate::api::session::SESSION_MANAGER;
use crate::exports::vpod::sandbox::executor::{
    EnvVar, ExecMode, ExecutionResult, Guest, MountEntry, SecretBinding as WitSecret, SliceOutput,
    TraceOptions,
};
use crate::vm;
use machine::virtio::secrets::SecretBinding;

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

type SplitSecrets = (Vec<(String, String)>, Vec<SecretBinding>);

fn split_secrets(secrets: Vec<WitSecret>) -> Result<SplitSecrets, String> {
    let mut guest_env = Vec::new();
    let mut bindings = Vec::new();

    for secret in secrets {
        if secret.placeholder.is_empty() {
            return Err(format!("secret {:?} has an empty placeholder", secret.name));
        }
        if secret.hosts.is_empty() {
            return Err(format!(
                "secret {:?} names no hosts, so it could never be used",
                secret.name
            ));
        }

        guest_env.push((secret.name, secret.placeholder.clone()));
        bindings.push(SecretBinding {
            placeholder: secret.placeholder,
            value: secret.value,
            hosts: secret.hosts,
        });
    }

    Ok((guest_env, bindings))
}

pub struct Executor;

impl Guest for Executor {
    fn session_start(
        snapshot_path: String,
        command: String,
        prompt: String,
        mounts: Vec<MountEntry>,
        env: Vec<EnvVar>,
        secrets: Vec<WitSecret>,
    ) -> Result<u64, String> {
        let mount_args: Vec<vm::MountArg> = mounts
            .into_iter()
            .map(|m| vm::MountArg {
                alias: m.host_alias,
                guest_path: m.guest_path,
                writable: m.writable,
            })
            .collect();

        let (placeholders, bindings) = split_secrets(secrets)?;
        let mut env = env_pairs(env)?;
        env.extend(placeholders);

        SESSION_MANAGER.start_session(snapshot_path, command, prompt, mount_args, env, bindings)
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
        secrets: Vec<WitSecret>,
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

        let (placeholders, bindings) = split_secrets(secrets)?;
        let mut env = env_pairs(env)?;
        env.extend(placeholders);

        SESSION_MANAGER.resume_session(
            snapshot_path,
            delta,
            command,
            prompt,
            mount_args,
            env,
            bindings,
        )
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
