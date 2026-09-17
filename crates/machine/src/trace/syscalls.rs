use std::collections::HashMap;

use riscv_core::{GuestString, SyscallEntry, SyscallKind};
use serde_json::Value;

use super::Tracer;

const VPOD_HELPER_PREFIX: &str = "/usr/lib/vpod/";

pub struct SyscallTracer {
    tracer: Tracer,
    trace_processes: bool,
    trace_files: bool,
    trace_network: bool,
    pending: HashMap<u64, (u64, SyscallKind)>,
    internal_tasks: HashMap<u64, bool>,
    bound_sockets: HashMap<(u64, u64), ([u8; 4], u16)>,
}

impl SyscallTracer {
    pub fn new(tracer: Tracer) -> Self {
        Self {
            trace_processes: tracer.traces_processes(),
            trace_files: tracer.traces_files(),
            trace_network: tracer.traces_network(),
            tracer,
            pending: HashMap::new(),
            internal_tasks: HashMap::new(),
            bound_sockets: HashMap::new(),
        }
    }

    pub fn on_entry(&mut self, entry: SyscallEntry) {
        match entry.kind {
            SyscallKind::Exec { path, argv } => self.emit_exec(entry.task, path, argv),
            SyscallKind::Exit { code } => self.emit_exit(entry.task, code),
            kind => {
                self.pending
                    .insert(entry.task, (entry.pc.wrapping_add(4), kind));
            }
        }
    }

    pub fn on_return(&mut self, task: u64, return_pc: u64, value: i64) {
        let Some((expected_pc, kind)) = self.pending.remove(&task) else {
            return;
        };

        if expected_pc != return_pc {
            return;
        }

        self.emit_return(task, kind, value);
    }

    fn is_internal(&self, task: u64) -> bool {
        self.internal_tasks.get(&task).copied().unwrap_or(false)
    }

    fn emit_exec(&mut self, task: u64, path: GuestString, argv: Vec<GuestString>) {
        let is_internal =
            matches!(&path, GuestString::Value(text) if text.starts_with(VPOD_HELPER_PREFIX));
        self.internal_tasks.insert(task, is_internal);

        if !self.trace_processes {
            return;
        }

        let mut fields = Vec::new();
        push_guest_string(
            &mut fields,
            "path",
            "path_truncated",
            "path_unreadable",
            path,
        );
        fields.push((
            "argv",
            Value::Array(argv.into_iter().map(guest_string_into_value).collect()),
        ));
        if is_internal {
            fields.push(("internal", true.into()));
        }
        self.tracer.record("process.exec", &fields);
    }

    fn emit_exit(&mut self, task: u64, code: i32) {
        let internal = self.internal_tasks.remove(&task).unwrap_or(false);
        if !self.trace_processes {
            return;
        }

        let mut fields = vec![("code", Value::from(code))];
        if internal {
            fields.push(("internal", true.into()));
        }
        self.tracer.record("process.exit", &fields);
    }

    fn emit_return(&mut self, task: u64, kind: SyscallKind, value: i64) {
        let internal = self.is_internal(task);

        match kind {
            SyscallKind::Clone { thread } => {
                if self.trace_processes && value > 0 {
                    let mut fields = vec![
                        ("child_pid", Value::from(value as u64)),
                        ("thread", Value::from(thread)),
                    ];
                    if internal {
                        fields.push(("internal", true.into()));
                    }
                    self.tracer.record("process.fork", &fields);
                }
            }
            SyscallKind::Open {
                path,
                write,
                read_write,
                create,
                truncate,
            } => {
                if !self.trace_files {
                    return;
                }
                let access = if read_write {
                    "read-write"
                } else if write {
                    "write"
                } else {
                    "read"
                };
                let mut fields = Vec::new();
                push_guest_string(
                    &mut fields,
                    "path",
                    "path_truncated",
                    "path_unreadable",
                    path,
                );
                fields.push(("access", access.into()));
                fields.push(("create", create.into()));
                fields.push(("truncate", truncate.into()));
                fields.push(("result", Value::from(value as i32)));
                if internal {
                    fields.push(("internal", true.into()));
                }
                self.tracer.record("file.open", &fields);
            }
            SyscallKind::Rename { from, to } => {
                if !self.trace_files {
                    return;
                }
                let mut fields = Vec::new();
                push_guest_string(
                    &mut fields,
                    "from",
                    "from_truncated",
                    "from_unreadable",
                    from,
                );
                push_guest_string(&mut fields, "to", "to_truncated", "to_unreadable", to);
                fields.push(("result", Value::from(value as i32)));
                if internal {
                    fields.push(("internal", true.into()));
                }
                self.tracer.record("file.rename", &fields);
            }
            SyscallKind::Unlink { path, directory } => {
                if !self.trace_files {
                    return;
                }
                let mut fields = Vec::new();
                push_guest_string(
                    &mut fields,
                    "path",
                    "path_truncated",
                    "path_unreadable",
                    path,
                );
                fields.push(("directory", directory.into()));
                fields.push(("result", Value::from(value as i32)));
                if internal {
                    fields.push(("internal", true.into()));
                }
                self.tracer.record("file.delete", &fields);
            }
            SyscallKind::Mkdir { path } => {
                if !self.trace_files {
                    return;
                }
                let mut fields = Vec::new();
                push_guest_string(
                    &mut fields,
                    "path",
                    "path_truncated",
                    "path_unreadable",
                    path,
                );
                fields.push(("result", Value::from(value as i32)));
                if internal {
                    fields.push(("internal", true.into()));
                }
                self.tracer.record("dir.create", &fields);
            }
            SyscallKind::Truncate { path, size } => {
                if !self.trace_files {
                    return;
                }
                let mut fields = Vec::new();
                push_guest_string(
                    &mut fields,
                    "path",
                    "path_truncated",
                    "path_unreadable",
                    path,
                );
                fields.push(("size", size.into()));
                fields.push(("result", Value::from(value as i32)));
                if internal {
                    fields.push(("internal", true.into()));
                }
                self.tracer.record("file.truncate", &fields);
            }
            SyscallKind::Connect {
                fd: _,
                address,
                port,
            } => {
                if !self.trace_network {
                    return;
                }
                let host = address.and_then(|address| self.tracer.name_of(address));
                let mut fields = vec![
                    ("protocol", Value::from("tcp")),
                    ("address", address.map(super::format_address).into()),
                    ("port", port.into()),
                    ("host", host.into()),
                    ("result", Value::from(value as i32)),
                ];
                if internal {
                    fields.push(("internal", true.into()));
                }
                self.tracer.record("net.connect", &fields);
            }
            SyscallKind::Bind { fd, address, port } => {
                if value == 0
                    && let Some(address) = address
                {
                    self.bound_sockets.insert((task, fd), (address, port));
                }
            }
            SyscallKind::Listen { fd } => {
                if value != 0 {
                    return;
                }
                let Some((address, port)) = self.bound_sockets.remove(&(task, fd)) else {
                    return;
                };
                if !self.trace_network {
                    return;
                }
                let host = self.tracer.name_of(address);
                let mut fields = vec![
                    ("protocol", Value::from("tcp")),
                    ("address", Value::from(super::format_address(address))),
                    ("port", port.into()),
                    ("host", host.into()),
                ];
                if internal {
                    fields.push(("internal", true.into()));
                }
                self.tracer.record("net.listen", &fields);
            }
            SyscallKind::Exec { .. } | SyscallKind::Exit { .. } => {
                unreachable!("exec and exit are emitted at entry, never pending")
            }
        }
    }
}

fn push_guest_string(
    fields: &mut Vec<(&'static str, Value)>,
    key: &'static str,
    truncated_key: &'static str,
    unreadable_key: &'static str,
    value: GuestString,
) {
    match value {
        GuestString::Value(text) => fields.push((key, text.into())),
        GuestString::Truncated(text) => {
            fields.push((key, text.into()));
            fields.push((truncated_key, true.into()));
        }
        GuestString::Unreadable => {
            fields.push((key, Value::Null));
            fields.push((unreadable_key, true.into()));
        }
    }
}

fn guest_string_into_value(value: GuestString) -> Value {
    match value {
        GuestString::Value(text) | GuestString::Truncated(text) => Value::from(text),
        GuestString::Unreadable => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::TraceOptions;

    fn guest_string(text: &str) -> GuestString {
        GuestString::Value(text.to_string())
    }

    fn entry(task: u64, pc: u64, kind: SyscallKind) -> SyscallEntry {
        SyscallEntry {
            task,
            number: 0,
            pc,
            kind,
        }
    }

    fn drained(tracer: &Tracer) -> Vec<Value> {
        String::from_utf8(tracer.drain(usize::MAX))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn an_open_is_only_emitted_once_its_matching_return_arrives() {
        let tracer = Tracer::new(TraceOptions::default());
        let mut syscalls = SyscallTracer::new(tracer.clone());

        syscalls.on_entry(entry(
            0x1000,
            0x4000,
            SyscallKind::Open {
                path: guest_string("/tmp/trace-demo.txt"),
                write: true,
                read_write: false,
                create: true,
                truncate: true,
            },
        ));
        assert!(drained(&tracer).is_empty());

        syscalls.on_return(0x1000, 0x4004, 3);

        let events = drained(&tracer);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "file.open");
        assert_eq!(events[0]["path"], "/tmp/trace-demo.txt");
        assert_eq!(events[0]["access"], "write");
        assert_eq!(events[0]["create"], true);
        assert_eq!(events[0]["result"], 3);
        assert!(events[0].get("internal").is_none());
    }

    #[test]
    fn a_return_at_the_wrong_pc_is_dropped_not_misattributed() {
        let tracer = Tracer::new(TraceOptions::default());
        let mut syscalls = SyscallTracer::new(tracer.clone());

        syscalls.on_entry(entry(
            0x1000,
            0x4000,
            SyscallKind::Mkdir {
                path: guest_string("/tmp/new-dir"),
            },
        ));
        // A signal or interrupt landed the hart back in U mode somewhere
        // else; this must not be read as sys_mkdirat's own return.
        syscalls.on_return(0x1000, 0x9999, 0);

        assert!(drained(&tracer).is_empty());
    }

    #[test]
    fn a_failed_call_is_still_recorded_with_its_negative_result() {
        let tracer = Tracer::new(TraceOptions::default());
        let mut syscalls = SyscallTracer::new(tracer.clone());

        syscalls.on_entry(entry(
            0x1000,
            0x4000,
            SyscallKind::Open {
                path: guest_string("/etc/shadow"),
                write: false,
                read_write: false,
                create: false,
                truncate: false,
            },
        ));
        syscalls.on_return(0x1000, 0x4004, -13); // EACCES

        let events = drained(&tracer);
        assert_eq!(events[0]["result"], -13);
    }

    #[test]
    fn a_process_run_from_the_vpod_helper_directory_is_marked_internal() {
        let tracer = Tracer::new(TraceOptions::default());
        let mut syscalls = SyscallTracer::new(tracer.clone());

        syscalls.on_entry(entry(
            0x1000,
            0x4000,
            SyscallKind::Exec {
                path: guest_string("/usr/lib/vpod/pyrunner.py"),
                argv: vec![guest_string("pyrunner.py")],
            },
        ));
        syscalls.on_entry(entry(
            0x1000,
            0x5000,
            SyscallKind::Mkdir {
                path: guest_string("/tmp/scratch"),
            },
        ));
        syscalls.on_return(0x1000, 0x5004, 0);

        let events = drained(&tracer);
        assert_eq!(events[0]["kind"], "process.exec");
        assert_eq!(events[0]["internal"], true);
        assert_eq!(events[1]["kind"], "dir.create");
        assert_eq!(events[1]["internal"], true);
    }

    #[test]
    fn a_user_command_is_not_marked_internal() {
        let tracer = Tracer::new(TraceOptions::default());
        let mut syscalls = SyscallTracer::new(tracer.clone());

        syscalls.on_entry(entry(
            0x1000,
            0x4000,
            SyscallKind::Exec {
                path: guest_string("/usr/bin/wget"),
                argv: vec![guest_string("wget")],
            },
        ));

        let events = drained(&tracer);
        assert!(events[0].get("internal").is_none());
    }

    #[test]
    fn a_process_exit_clears_its_task_so_the_pointer_can_be_reused() {
        let tracer = Tracer::new(TraceOptions::default());
        let mut syscalls = SyscallTracer::new(tracer.clone());

        syscalls.on_entry(entry(
            0x1000,
            0x4000,
            SyscallKind::Exec {
                path: guest_string("/usr/lib/vpod/pyrunner.py"),
                argv: vec![],
            },
        ));
        syscalls.on_entry(entry(0x1000, 0x5000, SyscallKind::Exit { code: 0 }));

        syscalls.on_entry(entry(
            0x1000,
            0x6000,
            SyscallKind::Exec {
                path: guest_string("/usr/bin/python3"),
                argv: vec![],
            },
        ));

        let events = drained(&tracer);
        assert_eq!(events.len(), 3);
        assert!(events[2].get("internal").is_none());
    }

    #[test]
    fn a_listen_is_paired_with_its_bind_address() {
        let tracer = Tracer::new(TraceOptions::default());
        let mut syscalls = SyscallTracer::new(tracer.clone());

        syscalls.on_entry(entry(
            0x1000,
            0x4000,
            SyscallKind::Bind {
                fd: 5,
                address: Some([0, 0, 0, 0]),
                port: 8080,
            },
        ));
        syscalls.on_return(0x1000, 0x4004, 0);
        assert!(drained(&tracer).is_empty(), "bind alone emits nothing");

        syscalls.on_entry(entry(0x1000, 0x5000, SyscallKind::Listen { fd: 5 }));
        syscalls.on_return(0x1000, 0x5004, 0);

        let events = drained(&tracer);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "net.listen");
        assert_eq!(events[0]["address"], "0.0.0.0");
        assert_eq!(events[0]["port"], 8080);
    }

    #[test]
    fn listen_on_a_socket_that_was_never_seen_bound_emits_nothing() {
        let tracer = Tracer::new(TraceOptions::default());
        let mut syscalls = SyscallTracer::new(tracer.clone());

        syscalls.on_entry(entry(0x1000, 0x4000, SyscallKind::Listen { fd: 5 }));
        syscalls.on_return(0x1000, 0x4004, 0);

        assert!(drained(&tracer).is_empty());
    }

    #[test]
    fn connect_looks_up_the_host_the_same_dns_answers_named() {
        let tracer = Tracer::new(TraceOptions::default());
        tracer.remember_name([151, 101, 0, 223], "pypi.org");
        let mut syscalls = SyscallTracer::new(tracer.clone());

        syscalls.on_entry(entry(
            0x1000,
            0x4000,
            SyscallKind::Connect {
                fd: 4,
                address: Some([151, 101, 0, 223]),
                port: 443,
            },
        ));
        syscalls.on_return(0x1000, 0x4004, 0);

        let events = drained(&tracer);
        assert_eq!(events[0]["kind"], "net.connect");
        assert_eq!(events[0]["host"], "pypi.org");
        assert_eq!(events[0]["address"], "151.101.0.223");
    }

    #[test]
    fn a_successful_fork_reports_the_child_pid() {
        let tracer = Tracer::new(TraceOptions::default());
        let mut syscalls = SyscallTracer::new(tracer.clone());

        syscalls.on_entry(entry(0x1000, 0x4000, SyscallKind::Clone { thread: false }));
        syscalls.on_return(0x1000, 0x4004, 4242);

        let events = drained(&tracer);
        assert_eq!(events[0]["kind"], "process.fork");
        assert_eq!(events[0]["child_pid"], 4242);
        assert_eq!(events[0]["thread"], false);
    }

    #[test]
    fn a_failed_fork_is_not_reported() {
        let tracer = Tracer::new(TraceOptions::default());
        let mut syscalls = SyscallTracer::new(tracer.clone());

        syscalls.on_entry(entry(0x1000, 0x4000, SyscallKind::Clone { thread: false }));
        syscalls.on_return(0x1000, 0x4004, -11); // EAGAIN

        assert!(drained(&tracer).is_empty());
    }

    #[test]
    fn disabling_files_still_traces_network() {
        let tracer = Tracer::new(TraceOptions {
            files: false,
            ..TraceOptions::default()
        });
        let mut syscalls = SyscallTracer::new(tracer.clone());

        syscalls.on_entry(entry(
            0x1000,
            0x4000,
            SyscallKind::Mkdir {
                path: guest_string("/tmp/x"),
            },
        ));
        syscalls.on_return(0x1000, 0x4004, 0);
        assert!(drained(&tracer).is_empty());

        syscalls.on_entry(entry(
            0x1000,
            0x5000,
            SyscallKind::Connect {
                fd: 4,
                address: None,
                port: 443,
            },
        ));
        syscalls.on_return(0x1000, 0x5004, 0);
        assert_eq!(drained(&tracer)[0]["kind"], "net.connect");
    }
}
