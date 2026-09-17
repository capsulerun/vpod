use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};

use riscv_core::{GuestString, SyscallEntry, SyscallKind};
use serde_json::Value;

use super::Tracer;

const VPOD_HELPER_PREFIX: &str = "/usr/lib/vpod/";
const VPOD_STAGING_PREFIX: &str = "/tmp/.vpod_";
const VPOD_DEVICES: [&str; 3] = ["/dev/ttyS1", "/dev/ttyS2", "/dev/ttyS3"];

const AF_INET: u32 = 2;
const AF_INET6: u32 = 10;
const SOCK_TYPE_MASK: u32 = 0xf;
const SOCK_STREAM: u32 = 1;
const SOCK_DGRAM: u32 = 2;

const PATH_KEYS: [&str; 3] = ["path", "path_truncated", "path_unreadable"];
const FROM_KEYS: [&str; 3] = ["from", "from_truncated", "from_unreadable"];
const TO_KEYS: [&str; 3] = ["to", "to_truncated", "to_unreadable"];

pub struct SyscallTracer {
    tracer: Tracer,
    trace_processes: bool,
    trace_files: bool,
    trace_network: bool,
    pending: HashMap<u64, Pending>,
    internal_tasks: HashSet<u64>,
    socket_protocols: HashMap<(u64, u64), &'static str>,
    bound_sockets: HashMap<(u64, u64), SocketAddr>,
}

struct Pending {
    return_pc: u64,
    kind: SyscallKind,
}

enum ExecOutcome {
    Succeeded,
    Failed(i64),
    Unknown,
}

impl SyscallTracer {
    pub fn new(tracer: Tracer) -> Self {
        Self {
            trace_processes: tracer.traces_processes(),
            trace_files: tracer.traces_files(),
            trace_network: tracer.traces_network(),
            tracer,
            pending: HashMap::new(),
            internal_tasks: HashSet::new(),
            socket_protocols: HashMap::new(),
            bound_sockets: HashMap::new(),
        }
    }

    pub fn on_entry(&mut self, entry: SyscallEntry) {
        if let SyscallKind::Exit { code } = entry.kind {
            self.emit_exit(entry.task, code);
            return;
        }

        self.pending.insert(
            entry.task,
            Pending {
                return_pc: entry.pc.wrapping_add(4),
                kind: entry.kind,
            },
        );
    }

    pub fn on_return(&mut self, task: u64, return_pc: u64, value: i64) {
        let Some(pending) = self.pending.remove(&task) else {
            return;
        };
        let returned_to_caller = return_pc == pending.return_pc;

        match pending.kind {
            SyscallKind::Exec {
                path,
                argv,
                argv_truncated,
            } => {
                let outcome = if returned_to_caller {
                    ExecOutcome::Failed(value)
                } else if value == 0 {
                    ExecOutcome::Succeeded
                } else {
                    ExecOutcome::Unknown
                };
                self.emit_exec(task, path, argv, argv_truncated, outcome);
            }
            kind if returned_to_caller => self.emit_return(task, kind, value),
            _ => {}
        }
    }

    fn emit_exec(
        &mut self,
        task: u64,
        path: GuestString,
        argv: Vec<String>,
        argv_truncated: bool,
        outcome: ExecOutcome,
    ) {
        let runs_vpod_plumbing = matches!(&path, GuestString::Value(text) if text.starts_with(VPOD_HELPER_PREFIX))
            || handles_only_staging_files(&argv);

        if let ExecOutcome::Succeeded = outcome {
            if runs_vpod_plumbing {
                self.internal_tasks.insert(task);
            } else {
                self.internal_tasks.remove(&task);
            }
        }

        if !self.trace_processes {
            return;
        }

        let internal = runs_vpod_plumbing || self.internal_tasks.contains(&task);
        let mut fields = task_fields(task);
        push_guest_string(&mut fields, PATH_KEYS, path);
        fields.push(("argv", argv.into()));
        if argv_truncated {
            fields.push(("argv_truncated", true.into()));
        }
        match outcome {
            ExecOutcome::Succeeded => {}
            ExecOutcome::Failed(value) => fields.push(("result", Value::from(value as i32))),
            ExecOutcome::Unknown => fields.push(("result", Value::Null)),
        }
        self.record("process.exec", fields, internal);
    }

    fn emit_exit(&mut self, task: u64, code: i32) {
        self.pending.remove(&task);
        self.socket_protocols.retain(|(owner, _), _| *owner != task);
        self.bound_sockets.retain(|(owner, _), _| *owner != task);
        let internal = self.internal_tasks.remove(&task);

        if !self.trace_processes {
            return;
        }

        let mut fields = task_fields(task);
        fields.push(("code", Value::from(code & 0xff)));
        self.record("process.exit", fields, internal);
    }

    fn emit_return(&mut self, task: u64, kind: SyscallKind, value: i64) {
        let internal_task = self.internal_tasks.contains(&task);
        let result = Value::from(value as i32);

        match kind {
            SyscallKind::Clone { thread } => {
                if !self.trace_processes || value <= 0 {
                    return;
                }
                let mut fields = task_fields(task);
                fields.push(("child_pid", Value::from(value)));
                fields.push(("thread", thread.into()));
                self.record("process.fork", fields, internal_task);
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
                let internal = internal_task || is_vpod_plumbing(&path);
                let mut fields = task_fields(task);
                push_guest_string(&mut fields, PATH_KEYS, path);
                fields.push(("access", access.into()));
                fields.push(("create", create.into()));
                fields.push(("truncate", truncate.into()));
                fields.push(("result", result));
                self.record("file.open", fields, internal);
            }
            SyscallKind::Rename { from, to } => {
                if !self.trace_files {
                    return;
                }
                let internal = internal_task || is_vpod_plumbing(&from) || is_vpod_plumbing(&to);
                let mut fields = task_fields(task);
                push_guest_string(&mut fields, FROM_KEYS, from);
                push_guest_string(&mut fields, TO_KEYS, to);
                fields.push(("result", result));
                self.record("file.rename", fields, internal);
            }
            SyscallKind::Unlink { path, directory } => {
                if !self.trace_files {
                    return;
                }
                let internal = internal_task || is_vpod_plumbing(&path);
                let mut fields = task_fields(task);
                push_guest_string(&mut fields, PATH_KEYS, path);
                fields.push(("directory", directory.into()));
                fields.push(("result", result));
                self.record("file.delete", fields, internal);
            }
            SyscallKind::Mkdir { path } => {
                if !self.trace_files {
                    return;
                }
                let internal = internal_task || is_vpod_plumbing(&path);
                let mut fields = task_fields(task);
                push_guest_string(&mut fields, PATH_KEYS, path);
                fields.push(("result", result));
                self.record("dir.create", fields, internal);
            }
            SyscallKind::Truncate { path, size } => {
                if !self.trace_files {
                    return;
                }
                let internal = internal_task || is_vpod_plumbing(&path);
                let mut fields = task_fields(task);
                push_guest_string(&mut fields, PATH_KEYS, path);
                fields.push(("size", size.into()));
                fields.push(("result", result));
                self.record("file.truncate", fields, internal);
            }
            SyscallKind::Socket {
                domain,
                socket_type,
            } => {
                let protocol = match socket_type & SOCK_TYPE_MASK {
                    SOCK_STREAM => "tcp",
                    SOCK_DGRAM => "udp",
                    _ => return,
                };
                if value >= 0 && matches!(domain, AF_INET | AF_INET6) {
                    self.socket_protocols.insert((task, value as u64), protocol);
                }
            }
            SyscallKind::Connect { fd, address } => {
                let Some(address) = address else {
                    return;
                };
                if !self.trace_network {
                    return;
                }
                let mut fields = task_fields(task);
                fields.push((
                    "protocol",
                    self.socket_protocols.get(&(task, fd)).copied().into(),
                ));
                fields.push(("address", address.ip().to_string().into()));
                fields.push(("port", address.port().into()));
                fields.push(("host", self.name_of(address.ip()).into()));
                fields.push(("result", result));
                self.record("net.connect", fields, internal_task);
            }
            SyscallKind::Bind { fd, address } => {
                if value == 0
                    && let Some(address) = address
                {
                    self.bound_sockets.insert((task, fd), address);
                }
            }
            SyscallKind::Listen { fd } => {
                if value != 0 {
                    return;
                }
                let Some(address) = self.bound_sockets.remove(&(task, fd)) else {
                    return;
                };
                if !self.trace_network {
                    return;
                }
                let mut fields = task_fields(task);
                fields.push((
                    "protocol",
                    self.socket_protocols.get(&(task, fd)).copied().into(),
                ));
                fields.push(("address", address.ip().to_string().into()));
                fields.push(("port", address.port().into()));
                self.record("net.listen", fields, internal_task);
            }
            SyscallKind::Exec { .. } | SyscallKind::Exit { .. } => {}
        }
    }

    fn name_of(&self, address: IpAddr) -> Option<String> {
        match address {
            IpAddr::V4(address) => self.tracer.name_of(address.octets()),
            IpAddr::V6(_) => None,
        }
    }

    fn record(&self, kind: &str, mut fields: Vec<(&'static str, Value)>, internal: bool) {
        if internal {
            fields.push(("internal", true.into()));
        }
        self.tracer.record(kind, &fields);
    }
}

fn task_fields(task: u64) -> Vec<(&'static str, Value)> {
    vec![("task", Value::from(format!("{task:x}")))]
}

fn handles_only_staging_files(argv: &[String]) -> bool {
    let Some((program, arguments)) = argv.split_first() else {
        return false;
    };
    let program = program.rsplit('/').next().unwrap_or(program);
    let mut operands = arguments
        .iter()
        .filter(|argument| !argument.starts_with('-'))
        .peekable();

    matches!(program, "base64" | "rm")
        && operands.peek().is_some()
        && operands.all(|operand| operand.starts_with(VPOD_STAGING_PREFIX))
}

fn is_vpod_plumbing(path: &GuestString) -> bool {
    matches!(path, GuestString::Value(text)
        if VPOD_DEVICES.contains(&text.as_str()) || text.starts_with(VPOD_STAGING_PREFIX))
}

fn push_guest_string(
    fields: &mut Vec<(&'static str, Value)>,
    [key, truncated_key, unreadable_key]: [&'static str; 3],
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::TraceOptions;

    const TASK: u64 = 0xffff_ffd8_0088_e600;
    const ECALL_PC: u64 = 0x4000;
    const AFTER_ECALL: u64 = ECALL_PC + 4;

    fn guest_string(text: &str) -> GuestString {
        GuestString::Value(text.to_string())
    }

    fn entry(kind: SyscallKind) -> SyscallEntry {
        SyscallEntry {
            task: TASK,
            number: 0,
            pc: ECALL_PC,
            kind,
        }
    }

    fn exec(path: &str, argv: &[&str]) -> SyscallKind {
        SyscallKind::Exec {
            path: guest_string(path),
            argv: argv.iter().map(|argument| argument.to_string()).collect(),
            argv_truncated: false,
        }
    }

    fn open(path: &str) -> SyscallKind {
        SyscallKind::Open {
            path: guest_string(path),
            write: true,
            read_write: false,
            create: true,
            truncate: true,
        }
    }

    fn traced() -> (Tracer, SyscallTracer) {
        traced_with(TraceOptions::default())
    }

    fn traced_with(options: TraceOptions) -> (Tracer, SyscallTracer) {
        let tracer = Tracer::new(options);
        let syscalls = SyscallTracer::new(tracer.clone());
        (tracer, syscalls)
    }

    fn call(syscalls: &mut SyscallTracer, kind: SyscallKind, value: i64) {
        syscalls.on_entry(entry(kind));
        syscalls.on_return(TASK, AFTER_ECALL, value);
    }

    fn succeed_exec(syscalls: &mut SyscallTracer, path: &str, argv: &[&str]) {
        syscalls.on_entry(entry(exec(path, argv)));
        syscalls.on_return(TASK, 0x1_0000, 0);
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
        let (tracer, mut syscalls) = traced();

        syscalls.on_entry(entry(open("/tmp/trace-demo.txt")));
        assert!(drained(&tracer).is_empty());

        syscalls.on_return(TASK, AFTER_ECALL, 3);

        let events = drained(&tracer);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "file.open");
        assert_eq!(events[0]["task"], "ffffffd80088e600");
        assert_eq!(events[0]["path"], "/tmp/trace-demo.txt");
        assert_eq!(events[0]["access"], "write");
        assert_eq!(events[0]["result"], 3);
        assert!(events[0].get("internal").is_none());
    }

    #[test]
    fn a_return_at_the_wrong_pc_is_dropped_not_misattributed() {
        let (tracer, mut syscalls) = traced();

        syscalls.on_entry(entry(SyscallKind::Mkdir {
            path: guest_string("/tmp/new-dir"),
        }));
        syscalls.on_return(TASK, 0x9999, 0);

        assert!(drained(&tracer).is_empty());
    }

    #[test]
    fn a_failed_call_is_still_recorded_with_its_negative_result() {
        let (tracer, mut syscalls) = traced();

        call(&mut syscalls, open("/etc/shadow"), -13); // EACCES

        assert_eq!(drained(&tracer)[0]["result"], -13);
    }

    #[test]
    fn a_successful_exec_is_recorded_when_the_task_resumes_in_the_new_program() {
        let (tracer, mut syscalls) = traced();

        succeed_exec(&mut syscalls, "/bin/sh", &["sh", "-c", "cd /app && make"]);

        let events = drained(&tracer);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "process.exec");
        assert_eq!(events[0]["path"], "/bin/sh");
        assert_eq!(
            events[0]["argv"],
            serde_json::json!(["sh", "-c", "cd /app && make"])
        );
        assert!(events[0].get("result").is_none());
        assert!(events[0].get("argv_truncated").is_none());
    }

    #[test]
    fn a_path_search_probe_that_fails_is_marked_with_its_error() {
        let (tracer, mut syscalls) = traced();

        call(
            &mut syscalls,
            exec("/usr/local/bin/git", &["git", "status"]),
            -2,
        ); // ENOENT
        succeed_exec(&mut syscalls, "/usr/bin/git", &["git", "status"]);

        let events = drained(&tracer);
        assert_eq!(events[0]["path"], "/usr/local/bin/git");
        assert_eq!(events[0]["result"], -2);
        assert_eq!(events[1]["path"], "/usr/bin/git");
        assert!(events[1].get("result").is_none());
    }

    #[test]
    fn an_exec_whose_return_is_redirected_to_a_signal_handler_has_an_unknown_result() {
        let (tracer, mut syscalls) = traced();

        syscalls.on_entry(entry(exec("/usr/bin/missing", &["missing"])));
        syscalls.on_return(TASK, 0x7777, 10); // SIGUSR1 handler, a0 = signal number

        let events = drained(&tracer);
        assert_eq!(events[0]["result"], Value::Null);
    }

    #[test]
    fn a_truncated_command_line_says_so() {
        let (tracer, mut syscalls) = traced();

        syscalls.on_entry(entry(SyscallKind::Exec {
            path: guest_string("/bin/sh"),
            argv: vec!["sh".into(), "-c".into(), "x".repeat(10)],
            argv_truncated: true,
        }));
        syscalls.on_return(TASK, 0x1_0000, 0);

        assert_eq!(drained(&tracer)[0]["argv_truncated"], true);
    }

    #[test]
    fn a_vpod_helper_is_internal_until_its_task_execs_something_else() {
        let (tracer, mut syscalls) = traced();

        succeed_exec(
            &mut syscalls,
            "/usr/lib/vpod/vpod-seed-entropy",
            &["vpod-seed-entropy"],
        );
        call(&mut syscalls, open("/tmp/seed"), 3);
        succeed_exec(&mut syscalls, "/usr/bin/wget", &["wget"]);
        call(&mut syscalls, open("/tmp/page.html"), 4);

        let events = drained(&tracer);
        assert_eq!(events[0]["internal"], true);
        assert_eq!(events[1]["internal"], true);
        assert!(events[2].get("internal").is_none());
        assert!(events[3].get("internal").is_none());
    }

    #[test]
    fn pyrunner_runs_user_code_so_its_activity_is_not_internal() {
        let (tracer, mut syscalls) = traced();

        succeed_exec(
            &mut syscalls,
            "/usr/bin/python3.real",
            &["/usr/bin/python3.real", "/usr/lib/vpod/pyrunner.py"],
        );
        call(&mut syscalls, open("/data/results.csv"), 3);

        let events = drained(&tracer);
        assert!(events.iter().all(|event| event.get("internal").is_none()));
    }

    #[test]
    fn opening_a_vpod_device_or_staging_file_is_internal_but_the_console_is_not() {
        let (tracer, mut syscalls) = traced();

        call(&mut syscalls, open("/dev/ttyS1"), 3);
        call(&mut syscalls, open("/tmp/.vpod_cmd.b64"), 3);
        call(&mut syscalls, open("/dev/ttyS0"), 3);

        let events = drained(&tracer);
        assert_eq!(events[0]["internal"], true);
        assert_eq!(events[1]["internal"], true);
        assert!(events[2].get("internal").is_none());
    }

    #[test]
    fn staging_a_long_command_is_internal_but_running_it_is_not() {
        let (tracer, mut syscalls) = traced();

        succeed_exec(
            &mut syscalls,
            "/bin/base64",
            &["base64", "-d", "/tmp/.vpod_cmd.b64"],
        );
        succeed_exec(
            &mut syscalls,
            "/bin/rm",
            &["rm", "-f", "/tmp/.vpod_cmd.b64"],
        );
        succeed_exec(&mut syscalls, "/bin/sh", &["sh", "/tmp/.vpod_cmd.sh"]);
        succeed_exec(
            &mut syscalls,
            "/bin/rm",
            &["rm", "-f", "/tmp/.vpod_cmd.b64", "/tmp/notes.txt"],
        );
        succeed_exec(&mut syscalls, "/bin/rm", &["rm", "-f"]);

        let events = drained(&tracer);
        assert_eq!(events[0]["internal"], true);
        assert_eq!(events[1]["internal"], true);
        assert!(events[2].get("internal").is_none());
        assert!(events[3].get("internal").is_none());
        assert!(events[4].get("internal").is_none());
    }

    #[test]
    fn an_exit_reports_the_status_the_shell_would_see_and_forgets_the_task() {
        let (tracer, mut syscalls) = traced();

        succeed_exec(
            &mut syscalls,
            "/usr/lib/vpod/vpod-seed-entropy",
            &["vpod-seed-entropy"],
        );
        syscalls.on_entry(entry(SyscallKind::Exit { code: 256 + 3 }));
        succeed_exec(&mut syscalls, "/usr/bin/python3", &["python3"]);

        let events = drained(&tracer);
        assert_eq!(events[1]["kind"], "process.exit");
        assert_eq!(events[1]["code"], 3);
        assert_eq!(events[1]["internal"], true);
        assert!(events[2].get("internal").is_none());
    }

    #[test]
    fn connect_reports_the_protocol_its_socket_was_created_with_and_the_dns_name() {
        let (tracer, mut syscalls) = traced();
        tracer.remember_name([151, 101, 0, 223], "pypi.org");

        call(
            &mut syscalls,
            SyscallKind::Socket {
                domain: AF_INET,
                socket_type: SOCK_STREAM | 0o4000, // SOCK_NONBLOCK
            },
            4,
        );
        call(
            &mut syscalls,
            SyscallKind::Connect {
                fd: 4,
                address: Some("151.101.0.223:443".parse().unwrap()),
            },
            -115, // EINPROGRESS
        );

        let events = drained(&tracer);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "net.connect");
        assert_eq!(events[0]["protocol"], "tcp");
        assert_eq!(events[0]["address"], "151.101.0.223");
        assert_eq!(events[0]["port"], 443);
        assert_eq!(events[0]["host"], "pypi.org");
        assert_eq!(events[0]["result"], -115);
    }

    #[test]
    fn a_connect_on_a_socket_we_never_saw_created_has_no_protocol() {
        let (tracer, mut syscalls) = traced();

        call(
            &mut syscalls,
            SyscallKind::Connect {
                fd: 9,
                address: Some("[2a04:4e42::223]:443".parse().unwrap()),
            },
            0,
        );

        let events = drained(&tracer);
        assert_eq!(events[0]["protocol"], Value::Null);
        assert_eq!(events[0]["address"], "2a04:4e42::223");
        assert_eq!(events[0]["host"], Value::Null);
    }

    #[test]
    fn a_unix_socket_connect_is_not_a_network_event() {
        let (tracer, mut syscalls) = traced();

        call(
            &mut syscalls,
            SyscallKind::Connect {
                fd: 3,
                address: None,
            },
            0,
        );

        assert!(drained(&tracer).is_empty());
    }

    #[test]
    fn a_listen_is_paired_with_its_bind_address() {
        let (tracer, mut syscalls) = traced();

        call(
            &mut syscalls,
            SyscallKind::Socket {
                domain: AF_INET,
                socket_type: SOCK_STREAM,
            },
            5,
        );
        call(
            &mut syscalls,
            SyscallKind::Bind {
                fd: 5,
                address: Some("0.0.0.0:8080".parse().unwrap()),
            },
            0,
        );
        assert!(
            drained(&tracer).is_empty(),
            "socket and bind alone emit nothing"
        );

        call(&mut syscalls, SyscallKind::Listen { fd: 5 }, 0);

        let events = drained(&tracer);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "net.listen");
        assert_eq!(events[0]["protocol"], "tcp");
        assert_eq!(events[0]["address"], "0.0.0.0");
        assert_eq!(events[0]["port"], 8080);
    }

    #[test]
    fn listen_on_a_socket_that_was_never_seen_bound_emits_nothing() {
        let (tracer, mut syscalls) = traced();

        call(&mut syscalls, SyscallKind::Listen { fd: 5 }, 0);

        assert!(drained(&tracer).is_empty());
    }

    #[test]
    fn a_successful_fork_reports_the_child_pid() {
        let (tracer, mut syscalls) = traced();

        call(&mut syscalls, SyscallKind::Clone { thread: false }, 4242);

        let events = drained(&tracer);
        assert_eq!(events[0]["kind"], "process.fork");
        assert_eq!(events[0]["child_pid"], 4242);
        assert_eq!(events[0]["thread"], false);
    }

    #[test]
    fn a_failed_fork_is_not_reported() {
        let (tracer, mut syscalls) = traced();

        call(&mut syscalls, SyscallKind::Clone { thread: false }, -11); // EAGAIN

        assert!(drained(&tracer).is_empty());
    }

    #[test]
    fn disabling_files_still_traces_network() {
        let (tracer, mut syscalls) = traced_with(TraceOptions {
            files: false,
            ..TraceOptions::default()
        });

        call(
            &mut syscalls,
            SyscallKind::Mkdir {
                path: guest_string("/tmp/x"),
            },
            0,
        );
        assert!(drained(&tracer).is_empty());

        call(
            &mut syscalls,
            SyscallKind::Connect {
                fd: 4,
                address: Some("10.0.2.2:443".parse().unwrap()),
            },
            0,
        );
        assert_eq!(drained(&tracer)[0]["kind"], "net.connect");
    }

    #[test]
    fn an_unreadable_path_is_null_and_flagged() {
        let (tracer, mut syscalls) = traced();

        call(
            &mut syscalls,
            SyscallKind::Rename {
                from: GuestString::Truncated("/tmp/aaaa".into()),
                to: GuestString::Unreadable,
            },
            -14, // EFAULT
        );

        let events = drained(&tracer);
        assert_eq!(events[0]["from"], "/tmp/aaaa");
        assert_eq!(events[0]["from_truncated"], true);
        assert_eq!(events[0]["to"], Value::Null);
        assert_eq!(events[0]["to_unreadable"], true);
    }
}
