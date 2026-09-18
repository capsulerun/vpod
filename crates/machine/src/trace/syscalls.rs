use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};

use riscv_core::{AT_FDCWD, GuestString, SyscallEntry, SyscallKind, SystemBus};
use serde_json::Value;

use super::Tracer;
use super::identity::Identities;
use super::processes::{Process, Processes};

const VPOD_HELPER_PREFIX: &str = "/usr/lib/vpod/";
const VPOD_STAGING_PREFIX: &str = "/tmp/.vpod_";
const VPOD_DEVICES: [&str; 3] = ["/dev/ttyS1", "/dev/ttyS2", "/dev/ttyS3"];

const AF_INET: u32 = 2;
const AF_INET6: u32 = 10;
const SOCK_TYPE_MASK: u32 = 0xf;
const SOCK_STREAM: u32 = 1;
const SOCK_DGRAM: u32 = 2;

const MAX_ADOPTION_DEPTH: usize = 8;
const MAX_TASKS_BY_ID: usize = 1024;

const PATH_KEYS: PathKeys = PathKeys {
    value: "path",
    truncated: "path_truncated",
    unreadable: "path_unreadable",
    unresolved: "path_unresolved",
};
const FROM_KEYS: PathKeys = PathKeys {
    value: "from",
    truncated: "from_truncated",
    unreadable: "from_unreadable",
    unresolved: "from_unresolved",
};
const TO_KEYS: PathKeys = PathKeys {
    value: "to",
    truncated: "to_truncated",
    unreadable: "to_unreadable",
    unresolved: "to_unresolved",
};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Owner {
    Process(u32),
    Task(u64),
}

impl Owner {
    fn process(self) -> Option<u32> {
        match self {
            Owner::Process(process_id) => Some(process_id),
            Owner::Task(_) => None,
        }
    }
}

struct PathKeys {
    value: &'static str,
    truncated: &'static str,
    unreadable: &'static str,
    unresolved: &'static str,
}

struct PathField {
    text: Option<String>,
    truncated: bool,
    unreadable: bool,
    unresolved: bool,
}

impl PathField {
    fn unknown() -> Self {
        Self {
            text: None,
            truncated: false,
            unreadable: false,
            unresolved: true,
        }
    }

    fn known(path: String) -> Self {
        Self {
            text: Some(path),
            truncated: false,
            unreadable: false,
            unresolved: false,
        }
    }

    fn resolved(&self) -> Option<&str> {
        match (&self.text, self.unresolved || self.truncated) {
            (Some(text), false) => Some(text),
            _ => None,
        }
    }
}

pub struct SyscallTracer {
    tracer: Tracer,
    trace_processes: bool,
    trace_files: bool,
    trace_network: bool,
    quiet: bool,
    reported_blind: bool,
    identities: Identities,
    processes: Processes,
    pending: HashMap<u64, Pending>,
    tasks_by_id: HashMap<u32, u64>,
    internal_owners: HashSet<Owner>,
    socket_protocols: HashMap<(Owner, i32), &'static str>,
    bound_sockets: HashMap<(Owner, i32), SocketAddr>,
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
            quiet: false,
            reported_blind: false,
            identities: Identities::default(),
            processes: Processes::default(),
            pending: HashMap::new(),
            tasks_by_id: HashMap::new(),
            internal_owners: HashSet::new(),
            socket_protocols: HashMap::new(),
            bound_sockets: HashMap::new(),
        }
    }

    pub fn set_quiet(&mut self, quiet: bool) {
        self.quiet = quiet;
    }

    pub fn knows_process_ids(&self) -> bool {
        self.identities.calibrated()
    }

    pub fn seed_working_directory(&mut self, process_id: u32, path: String) {
        self.processes.set_working_directory(process_id, path);
    }

    pub fn on_entry<B: SystemBus>(&mut self, entry: SyscallEntry, bus: &mut B, satp: u64) {
        match entry.kind {
            SyscallKind::Exit { code } => {
                self.emit_exit(entry.task, code, bus, satp);
                return;
            }
            SyscallKind::Identity { .. } if self.identities.knows_parents() => return,
            _ => {}
        }

        self.pending.insert(
            entry.task,
            Pending {
                return_pc: entry.pc.wrapping_add(4),
                kind: entry.kind,
            },
        );
    }

    pub fn on_return<B: SystemBus>(
        &mut self,
        task: u64,
        return_pc: u64,
        value: i64,
        bus: &mut B,
        satp: u64,
    ) {
        let Some(pending) = self.pending.remove(&task) else {
            return;
        };
        let returned_to_caller = return_pc == pending.return_pc;

        match pending.kind {
            SyscallKind::Exec {
                directory_fd,
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
                self.finish_exec(
                    task,
                    directory_fd,
                    path,
                    argv,
                    argv_truncated,
                    outcome,
                    bus,
                    satp,
                );
            }
            kind if returned_to_caller => self.finish(task, kind, value, bus, satp),
            _ => {}
        }
    }

    fn finish<B: SystemBus>(
        &mut self,
        task: u64,
        kind: SyscallKind,
        value: i64,
        bus: &mut B,
        satp: u64,
    ) {
        let owner = self.owner_of(task, bus, satp);
        let internal_owner = self.internal_owners.contains(&owner);
        let succeeded = value >= 0;
        let result = Value::from(value as i32);

        match kind {
            SyscallKind::Identity { .. } => {
                if value > 0 {
                    self.observe_identity(task, value as u32, bus, satp);
                }
            }
            SyscallKind::RingUse => {
                if !succeeded || self.reported_blind {
                    return;
                }
                self.reported_blind = true;
                let mut fields = identity_fields(owner, task);
                fields.push(("reason", "io-uring".into()));
                self.record("trace.blind", fields, false);
            }
            SyscallKind::Clone { thread } => {
                if value <= 0 {
                    return;
                }
                let child_id = value as u32;
                if !thread {
                    self.record_fork(owner, task, child_id);
                    if internal_owner {
                        self.internal_owners.insert(Owner::Process(child_id));
                    }
                }
                self.pair_parent(child_id, task, bus, satp);

                if !self.trace_processes {
                    return;
                }
                let mut fields = identity_fields(owner, task);
                fields.push(("child_pid", Value::from(child_id)));
                fields.push(("thread", thread.into()));
                self.record("process.fork", fields, internal_owner);
            }
            SyscallKind::Open {
                directory_fd,
                path,
                write,
                read_write,
                create,
                truncate,
                close_on_exec,
            } => {
                let field = self.path_field(owner, directory_fd, path);
                if succeeded
                    && let Some(text) = field.resolved().map(str::to_string)
                    && let Some(process) = self.process_mut(owner)
                {
                    process.open(value as i32, text, close_on_exec);
                }

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
                let internal = internal_owner || is_vpod_plumbing(&field);
                let mut fields = identity_fields(owner, task);
                push_path(&mut fields, &PATH_KEYS, field);
                fields.push(("access", access.into()));
                fields.push(("create", create.into()));
                fields.push(("truncate", truncate.into()));
                fields.push(("result", result));
                self.record("file.open", fields, internal);
            }
            SyscallKind::Rename {
                from_directory_fd,
                from,
                to_directory_fd,
                to,
            } => {
                let from = self.path_field(owner, from_directory_fd, from);
                let to = self.path_field(owner, to_directory_fd, to);
                if !self.trace_files {
                    return;
                }
                let internal = internal_owner || is_vpod_plumbing(&from) || is_vpod_plumbing(&to);
                let mut fields = identity_fields(owner, task);
                push_path(&mut fields, &FROM_KEYS, from);
                push_path(&mut fields, &TO_KEYS, to);
                fields.push(("result", result));
                self.record("file.rename", fields, internal);
            }
            SyscallKind::Unlink {
                directory_fd,
                path,
                directory,
            } => {
                let field = self.path_field(owner, directory_fd, path);
                if !self.trace_files {
                    return;
                }
                let internal = internal_owner || is_vpod_plumbing(&field);
                let mut fields = identity_fields(owner, task);
                push_path(&mut fields, &PATH_KEYS, field);
                fields.push(("directory", directory.into()));
                fields.push(("result", result));
                self.record("file.delete", fields, internal);
            }
            SyscallKind::Mkdir { directory_fd, path } => {
                let field = self.path_field(owner, directory_fd, path);
                if !self.trace_files {
                    return;
                }
                let internal = internal_owner || is_vpod_plumbing(&field);
                let mut fields = identity_fields(owner, task);
                push_path(&mut fields, &PATH_KEYS, field);
                fields.push(("result", result));
                self.record("dir.create", fields, internal);
            }
            SyscallKind::Truncate { path, size } => {
                let field = self.path_field(owner, AT_FDCWD, path);
                self.emit_truncate(owner, task, field, size, result, internal_owner);
            }
            SyscallKind::TruncateDescriptor { fd, size } => {
                let field = match self.processes.path_of_descriptor(owner.process(), fd) {
                    Some(path) => PathField::known(path),
                    None => PathField::unknown(),
                };
                self.emit_truncate(owner, task, field, size, result, internal_owner);
            }
            SyscallKind::ChangeDirectory { path } => {
                if value != 0 {
                    return;
                }
                let field = self.path_field(owner, AT_FDCWD, path);
                if let Some(text) = field.resolved().map(str::to_string)
                    && let Some(process) = self.process_mut(owner)
                {
                    process.working_directory = Some(text);
                }
            }
            SyscallKind::ChangeDirectoryDescriptor { fd } => {
                if value != 0 {
                    return;
                }
                if let Some(path) = self.processes.path_of_descriptor(owner.process(), fd)
                    && let Some(process) = self.process_mut(owner)
                {
                    process.working_directory = Some(path);
                }
            }
            SyscallKind::Duplicate { fd, close_on_exec } => {
                if succeeded && let Some(process) = self.process_mut(owner) {
                    process.duplicate(fd, value as i32, close_on_exec);
                }
            }
            SyscallKind::DuplicateTo {
                from_fd,
                to_fd,
                close_on_exec,
            } => {
                if succeeded && let Some(process) = self.process_mut(owner) {
                    process.duplicate(from_fd, to_fd, close_on_exec);
                }
            }
            SyscallKind::Close { fd } => {
                if !succeeded {
                    return;
                }
                if let Some(process) = self.process_mut(owner) {
                    process.close(fd);
                }
                self.socket_protocols.remove(&(owner, fd));
                self.bound_sockets.remove(&(owner, fd));
            }
            SyscallKind::CloseRange { first, last } => {
                if !succeeded {
                    return;
                }
                if let Some(process) = self.process_mut(owner) {
                    process.close_range(first, last);
                }
                let range = first as i32..=last.min(i32::MAX as u32) as i32;
                self.socket_protocols
                    .retain(|(key, fd), _| *key != owner || !range.contains(fd));
                self.bound_sockets
                    .retain(|(key, fd), _| *key != owner || !range.contains(fd));
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
                if succeeded && matches!(domain, AF_INET | AF_INET6) {
                    self.socket_protocols
                        .insert((owner, value as i32), protocol);
                }
                if succeeded && let Some(process) = self.process_mut(owner) {
                    process.close(value as i32);
                }
            }
            SyscallKind::Connect { fd, address } => {
                let Some(address) = address else {
                    return;
                };
                if !self.trace_network {
                    return;
                }
                let mut fields = identity_fields(owner, task);
                fields.push((
                    "protocol",
                    self.socket_protocols.get(&(owner, fd)).copied().into(),
                ));
                fields.push(("address", address.ip().to_string().into()));
                fields.push(("port", address.port().into()));
                fields.push(("host", self.name_of(address.ip()).into()));
                fields.push(("result", result));
                self.record("net.connect", fields, internal_owner);
            }
            SyscallKind::Bind { fd, address } => {
                if value == 0
                    && let Some(address) = address
                {
                    self.bound_sockets.insert((owner, fd), address);
                }
            }
            SyscallKind::Listen { fd } => {
                if value != 0 {
                    return;
                }
                let Some(address) = self.bound_sockets.remove(&(owner, fd)) else {
                    return;
                };
                if !self.trace_network {
                    return;
                }
                let mut fields = identity_fields(owner, task);
                fields.push((
                    "protocol",
                    self.socket_protocols.get(&(owner, fd)).copied().into(),
                ));
                fields.push(("address", address.ip().to_string().into()));
                fields.push(("port", address.port().into()));
                self.record("net.listen", fields, internal_owner);
            }
            SyscallKind::Exec { .. } | SyscallKind::Exit { .. } => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_exec<B: SystemBus>(
        &mut self,
        task: u64,
        directory_fd: i32,
        path: GuestString,
        argv: Vec<String>,
        argv_truncated: bool,
        outcome: ExecOutcome,
        bus: &mut B,
        satp: u64,
    ) {
        let owner = self.owner_of(task, bus, satp);
        let field = self.path_field(owner, directory_fd, path);
        let runs_vpod_plumbing = field
            .resolved()
            .is_some_and(|text| text.starts_with(VPOD_HELPER_PREFIX))
            || handles_only_staging_files(&argv);

        if let ExecOutcome::Succeeded = outcome {
            if runs_vpod_plumbing {
                self.internal_owners.insert(owner);
            } else {
                self.internal_owners.remove(&owner);
            }
            if let Some(process) = self.process_mut(owner) {
                process.keep_across_exec();
            }
        }

        if !self.trace_processes {
            return;
        }

        let internal = runs_vpod_plumbing || self.internal_owners.contains(&owner);
        let mut fields = identity_fields(owner, task);
        if let Some(parent) = self.parent_of(owner) {
            fields.push(("ppid", Value::from(parent)));
        }
        push_path(&mut fields, &PATH_KEYS, field);
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

    fn emit_exit<B: SystemBus>(&mut self, task: u64, code: i32, bus: &mut B, satp: u64) {
        let owner = self.owner_of(task, bus, satp);
        self.pending.remove(&task);
        self.socket_protocols.retain(|(key, _), _| *key != owner);
        self.bound_sockets.retain(|(key, _), _| *key != owner);
        let internal = self.internal_owners.remove(&owner);
        if let Some(process_id) = owner.process() {
            self.processes.remove(process_id);
        }

        if !self.trace_processes {
            return;
        }

        let mut fields = identity_fields(owner, task);
        fields.push(("code", Value::from(code & 0xff)));
        self.record("process.exit", fields, internal);
    }

    fn emit_truncate(
        &self,
        owner: Owner,
        task: u64,
        field: PathField,
        size: u64,
        result: Value,
        internal_owner: bool,
    ) {
        if !self.trace_files {
            return;
        }
        let internal = internal_owner || is_vpod_plumbing(&field);
        let mut fields = identity_fields(owner, task);
        push_path(&mut fields, &PATH_KEYS, field);
        fields.push(("size", size.into()));
        fields.push(("result", result));
        self.record("file.truncate", fields, internal);
    }

    fn owner_of<B: SystemBus>(&mut self, task: u64, bus: &mut B, satp: u64) -> Owner {
        match self.identities.process_of(task, bus, satp) {
            Some(process_id) => {
                self.adopt(process_id, task, bus, satp, 0);
                Owner::Process(process_id)
            }
            None => Owner::Task(task),
        }
    }

    fn adopt<B: SystemBus>(
        &mut self,
        process_id: u32,
        task: u64,
        bus: &mut B,
        satp: u64,
        depth: usize,
    ) {
        if let Some(fork) = self.processes.take_fork(process_id) {
            self.processes.insert(process_id, fork.state);
            return;
        }
        if self.processes.contains(process_id) {
            return;
        }
        if depth >= MAX_ADOPTION_DEPTH {
            self.processes.insert(process_id, Process::default());
            return;
        }

        let parent_task = self.identities.parent_task_of(task, bus, satp);
        let parent_id =
            parent_task.and_then(|parent| self.identities.process_of(parent, bus, satp));
        let forking = parent_task.is_some_and(|parent| self.is_cloning(parent));
        let mut state = Process {
            parent: parent_id,
            ..Process::default()
        };

        if let (true, Some(parent_task), Some(parent_id)) = (forking, parent_task, parent_id) {
            self.adopt(parent_id, parent_task, bus, satp, depth + 1);
            if let Some(parent) = self.processes.get(parent_id) {
                state.working_directory = parent.working_directory.clone();
                state.descriptors = parent.descriptors.clone();
            }
            if self.internal_owners.contains(&Owner::Process(parent_id)) {
                self.internal_owners.insert(Owner::Process(process_id));
            }
        }

        self.processes.insert(process_id, state);
    }

    fn is_cloning(&self, task: u64) -> bool {
        matches!(self.pending.get(&task), Some(pending)
            if matches!(pending.kind, SyscallKind::Clone { .. }))
    }

    fn record_fork(&mut self, owner: Owner, task: u64, child_id: u32) {
        let mut state = owner
            .process()
            .and_then(|process_id| self.processes.get(process_id))
            .cloned()
            .unwrap_or_default();
        state.parent = owner.process();
        self.processes.record_fork(child_id, state, task);
    }

    fn pair_parent<B: SystemBus>(
        &mut self,
        child_id: u32,
        parent_task: u64,
        bus: &mut B,
        satp: u64,
    ) {
        if self.identities.knows_parents() {
            return;
        }
        if let Some(child_task) = self.tasks_by_id.remove(&child_id)
            && self.identities.thread_of(child_task, bus, satp) == Some(child_id)
        {
            self.identities
                .observe_parent(child_task, parent_task, bus, satp);
        }
    }

    fn observe_identity<B: SystemBus>(&mut self, task: u64, value: u32, bus: &mut B, satp: u64) {
        self.identities.observe_identity(task, value, bus, satp);

        if self.identities.knows_parents() {
            self.tasks_by_id = HashMap::new();
            return;
        }

        match self.processes.parent_task_of_fork(value) {
            Some(parent_task) => self.identities.observe_parent(task, parent_task, bus, satp),
            None => {
                if self.tasks_by_id.len() >= MAX_TASKS_BY_ID {
                    self.tasks_by_id = HashMap::new();
                }
                self.tasks_by_id.insert(value, task);
            }
        }
    }

    fn process_mut(&mut self, owner: Owner) -> Option<&mut Process> {
        self.processes.get_mut(owner.process()?)
    }

    fn parent_of(&self, owner: Owner) -> Option<u32> {
        self.processes.get(owner.process()?)?.parent
    }

    fn path_field(&self, owner: Owner, directory_fd: i32, path: GuestString) -> PathField {
        let (text, truncated) = match path {
            GuestString::Unreadable => {
                return PathField {
                    text: None,
                    truncated: false,
                    unreadable: true,
                    unresolved: false,
                };
            }
            GuestString::Value(text) => (text, false),
            GuestString::Truncated(text) => (text, true),
        };

        match self.processes.resolve(owner.process(), directory_fd, &text) {
            Some(resolved) => PathField {
                text: Some(resolved),
                truncated,
                unreadable: false,
                unresolved: false,
            },
            None => PathField {
                text: Some(text),
                truncated,
                unreadable: false,
                unresolved: true,
            },
        }
    }

    fn name_of(&self, address: IpAddr) -> Option<String> {
        match address {
            IpAddr::V4(address) => self.tracer.name_of(address.octets()),
            IpAddr::V6(_) => None,
        }
    }

    fn record(&self, kind: &str, mut fields: Vec<(&'static str, Value)>, internal: bool) {
        if self.quiet {
            return;
        }
        if internal {
            fields.push(("internal", true.into()));
        }
        self.tracer.record(kind, &fields);
    }
}

fn identity_fields(owner: Owner, task: u64) -> Vec<(&'static str, Value)> {
    vec![
        ("task", Value::from(format!("{task:x}"))),
        ("pid", owner.process().map_or(Value::Null, Value::from)),
    ]
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

fn is_vpod_plumbing(field: &PathField) -> bool {
    field
        .resolved()
        .is_some_and(|text| VPOD_DEVICES.contains(&text) || text.starts_with(VPOD_STAGING_PREFIX))
}

fn push_path(fields: &mut Vec<(&'static str, Value)>, keys: &PathKeys, field: PathField) {
    fields.push((keys.value, field.text.map_or(Value::Null, Value::from)));
    if field.truncated {
        fields.push((keys.truncated, true.into()));
    }
    if field.unreadable {
        fields.push((keys.unreadable, true.into()));
    }
    if field.unresolved {
        fields.push((keys.unresolved, true.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::TraceOptions;
    use riscv_core::FlatMemory;

    const PID_OFFSET: u64 = 1296;
    const REAL_PARENT_OFFSET: u64 = 1312;
    const KERNEL_BASE: u64 = 0xffff_ffd6_0000_0000;
    const GUEST_BYTES: u64 = 32 * 1024 * 1024;
    const ECALL_PC: u64 = 0x4000;
    const AFTER_ECALL: u64 = ECALL_PC + 4;
    const SHELL: u32 = 1;

    struct Guest {
        memory: FlatMemory,
        tracer: Tracer,
        syscalls: SyscallTracer,
    }

    impl Guest {
        fn new() -> Self {
            Self::with_options(TraceOptions::default())
        }

        fn with_options(options: TraceOptions) -> Self {
            let tracer = Tracer::new(options);
            let syscalls = SyscallTracer::new(tracer.clone());
            let mut guest = Self {
                memory: FlatMemory::new(GUEST_BYTES as usize),
                tracer,
                syscalls,
            };
            guest.spawn(SHELL, SHELL, 0);
            guest
        }

        fn task_of(process_id: u32) -> u64 {
            KERNEL_BASE + 0x10000 + process_id as u64 * 0x4000
        }

        fn slot(address: u64) -> usize {
            (address & (GUEST_BYTES - 1)) as usize
        }

        fn spawn(&mut self, process_id: u32, thread_id: u32, parent: u32) {
            let task = Self::task_of(process_id);
            let parent_task = if parent == 0 {
                0
            } else {
                Self::task_of(parent)
            };
            self.memory
                .load_at(Self::slot(task + PID_OFFSET), &thread_id.to_le_bytes());
            self.memory
                .load_at(Self::slot(task + PID_OFFSET + 4), &process_id.to_le_bytes());
            self.memory.load_at(
                Self::slot(task + REAL_PARENT_OFFSET),
                &parent_task.to_le_bytes(),
            );
        }

        fn calibrate(&mut self) {
            for process_id in [201u32, 202] {
                self.spawn(process_id, process_id, 0);
                self.call(
                    process_id,
                    SyscallKind::Identity { group: false },
                    process_id as i64,
                );
            }
            assert!(self.syscalls.knows_process_ids());

            for (parent, child) in [(203u32, 204u32), (205, 206)] {
                self.spawn(parent, parent, 0);
                self.fork_running_child_first(
                    parent,
                    child,
                    SyscallKind::Identity { group: false },
                    child as i64,
                );
            }
            assert!(self.syscalls.identities.knows_parents());
            let _ = self.events();
        }

        fn spawn_thread(&mut self, thread_id: u32, process_id: u32) {
            let task = Self::task_of(thread_id);
            self.memory
                .load_at(Self::slot(task + PID_OFFSET), &thread_id.to_le_bytes());
            self.memory
                .load_at(Self::slot(task + PID_OFFSET + 4), &process_id.to_le_bytes());
        }

        fn seed(&mut self, process_id: u32, path: &str) {
            self.syscalls
                .seed_working_directory(process_id, path.to_string());
        }

        fn call(&mut self, process_id: u32, kind: SyscallKind, value: i64) {
            self.call_on_task(Self::task_of(process_id), kind, value);
        }

        fn call_on_task(&mut self, task: u64, kind: SyscallKind, value: i64) {
            self.syscalls
                .on_entry(entry(task, kind), &mut self.memory, 0);
            self.syscalls
                .on_return(task, AFTER_ECALL, value, &mut self.memory, 0);
        }

        fn exec(&mut self, process_id: u32, path: &str, argv: &[&str]) {
            let task = Self::task_of(process_id);
            self.syscalls
                .on_entry(entry(task, exec_kind(path, argv)), &mut self.memory, 0);
            self.syscalls
                .on_return(task, 0x1_0000, 0, &mut self.memory, 0);
        }

        fn fork(&mut self, parent: u32, child: u32) {
            self.spawn(child, child, parent);
            self.call(parent, SyscallKind::Clone { thread: false }, child as i64);
        }

        fn fork_running_child_first(
            &mut self,
            parent: u32,
            child: u32,
            inside_child: SyscallKind,
            value: i64,
        ) {
            self.spawn(child, child, parent);
            let parent_task = Self::task_of(parent);
            self.syscalls.on_entry(
                entry(parent_task, SyscallKind::Clone { thread: false }),
                &mut self.memory,
                0,
            );
            self.call(child, inside_child, value);
            self.syscalls
                .on_return(parent_task, AFTER_ECALL, child as i64, &mut self.memory, 0);
        }

        fn events(&self) -> Vec<Value> {
            String::from_utf8(self.tracer.drain(usize::MAX))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }
    }

    fn entry(task: u64, kind: SyscallKind) -> SyscallEntry {
        SyscallEntry {
            task,
            number: 0,
            pc: ECALL_PC,
            kind,
        }
    }

    fn exec_kind(path: &str, argv: &[&str]) -> SyscallKind {
        SyscallKind::Exec {
            directory_fd: AT_FDCWD,
            path: GuestString::Value(path.to_string()),
            argv: argv.iter().map(|argument| argument.to_string()).collect(),
            argv_truncated: false,
        }
    }

    fn open(path: &str) -> SyscallKind {
        open_at(AT_FDCWD, path)
    }

    fn open_at(directory_fd: i32, path: &str) -> SyscallKind {
        SyscallKind::Open {
            directory_fd,
            path: GuestString::Value(path.to_string()),
            write: true,
            read_write: false,
            create: true,
            truncate: true,
            close_on_exec: false,
        }
    }

    #[test]
    fn an_open_is_only_emitted_once_its_matching_return_arrives() {
        let mut guest = Guest::new();
        guest.calibrate();
        let task = Guest::task_of(SHELL);

        guest.syscalls.on_entry(
            entry(task, open("/tmp/trace-demo.txt")),
            &mut guest.memory,
            0,
        );
        assert!(guest.events().is_empty());

        guest
            .syscalls
            .on_return(task, AFTER_ECALL, 3, &mut guest.memory, 0);

        let events = guest.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "file.open");
        assert_eq!(events[0]["pid"], 1);
        assert_eq!(events[0]["path"], "/tmp/trace-demo.txt");
        assert_eq!(events[0]["access"], "write");
        assert_eq!(events[0]["result"], 3);
        assert!(events[0].get("internal").is_none());
    }

    #[test]
    fn a_return_at_the_wrong_pc_is_dropped_not_misattributed() {
        let mut guest = Guest::new();
        let task = Guest::task_of(SHELL);

        guest.syscalls.on_entry(
            entry(
                task,
                SyscallKind::Mkdir {
                    directory_fd: AT_FDCWD,
                    path: GuestString::Value("/tmp/new-dir".into()),
                },
            ),
            &mut guest.memory,
            0,
        );
        guest
            .syscalls
            .on_return(task, 0x9999, 0, &mut guest.memory, 0);

        assert!(guest.events().is_empty());
    }

    #[test]
    fn a_failed_call_is_still_recorded_with_its_negative_result() {
        let mut guest = Guest::new();
        guest.call(SHELL, open("/etc/shadow"), -13); // EACCES

        assert_eq!(guest.events()[0]["result"], -13);
    }

    #[test]
    fn events_carry_no_process_id_until_the_offsets_are_calibrated() {
        let mut guest = Guest::new();
        guest.call(SHELL, open("/tmp/early.txt"), 3);

        let events = guest.events();
        assert_eq!(events[0]["pid"], Value::Null);
        assert_eq!(events[0]["task"], format!("{:x}", Guest::task_of(SHELL)));
    }

    #[test]
    fn a_successful_exec_is_recorded_when_the_task_resumes_in_the_new_program() {
        let mut guest = Guest::new();
        guest.calibrate();
        guest.exec(SHELL, "/bin/sh", &["sh", "-c", "cd /app && make"]);

        let events = guest.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "process.exec");
        assert_eq!(events[0]["path"], "/bin/sh");
        assert_eq!(
            events[0]["argv"],
            serde_json::json!(["sh", "-c", "cd /app && make"])
        );
        assert!(events[0].get("result").is_none());
    }

    #[test]
    fn an_exec_names_the_process_that_started_it() {
        let mut guest = Guest::new();
        guest.calibrate();
        guest.fork(SHELL, 700);
        guest.exec(700, "/bin/cat", &["cat", "notes.txt"]);

        let events = guest.events();
        let exec = events.last().unwrap();
        assert_eq!(exec["pid"], 700);
        assert_eq!(exec["ppid"], 1);
    }

    #[test]
    fn a_path_search_probe_that_fails_is_marked_with_its_error() {
        let mut guest = Guest::new();
        guest.calibrate();

        guest.call(
            SHELL,
            exec_kind("/usr/local/bin/git", &["git", "status"]),
            -2,
        );
        guest.exec(SHELL, "/usr/bin/git", &["git", "status"]);

        let events = guest.events();
        assert_eq!(events[0]["path"], "/usr/local/bin/git");
        assert_eq!(events[0]["result"], -2);
        assert_eq!(events[1]["path"], "/usr/bin/git");
        assert!(events[1].get("result").is_none());
    }

    #[test]
    fn an_exec_whose_return_is_redirected_to_a_signal_handler_has_an_unknown_result() {
        let mut guest = Guest::new();
        let task = Guest::task_of(SHELL);

        guest.syscalls.on_entry(
            entry(task, exec_kind("/usr/bin/missing", &["missing"])),
            &mut guest.memory,
            0,
        );
        guest
            .syscalls
            .on_return(task, 0x7777, 10, &mut guest.memory, 0);

        assert_eq!(guest.events()[0]["result"], Value::Null);
    }

    #[test]
    fn a_truncated_command_line_says_so() {
        let mut guest = Guest::new();
        let task = Guest::task_of(SHELL);

        guest.syscalls.on_entry(
            entry(
                task,
                SyscallKind::Exec {
                    directory_fd: AT_FDCWD,
                    path: GuestString::Value("/bin/sh".into()),
                    argv: vec!["sh".into(), "-c".into(), "x".repeat(10)],
                    argv_truncated: true,
                },
            ),
            &mut guest.memory,
            0,
        );
        guest
            .syscalls
            .on_return(task, 0x1_0000, 0, &mut guest.memory, 0);

        assert_eq!(guest.events()[0]["argv_truncated"], true);
    }

    #[test]
    fn a_vpod_helper_is_internal_until_its_task_execs_something_else() {
        let mut guest = Guest::new();
        guest.calibrate();

        guest.exec(
            SHELL,
            "/usr/lib/vpod/vpod-seed-entropy",
            &["vpod-seed-entropy"],
        );
        guest.call(SHELL, open("/tmp/seed"), 3);
        guest.exec(SHELL, "/usr/bin/wget", &["wget"]);
        guest.call(SHELL, open("/tmp/page.html"), 4);

        let events = guest.events();
        assert_eq!(events[0]["internal"], true);
        assert_eq!(events[1]["internal"], true);
        assert!(events[2].get("internal").is_none());
        assert!(events[3].get("internal").is_none());
    }

    #[test]
    fn a_helper_that_forks_keeps_its_children_internal_too() {
        let mut guest = Guest::new();
        guest.calibrate();

        guest.exec(
            SHELL,
            "/usr/lib/vpod/vpod-seed-entropy",
            &["vpod-seed-entropy"],
        );
        guest.fork(SHELL, 800);
        guest.call(800, open("/tmp/helper-child"), 3);

        let events = guest.events();
        assert_eq!(events.last().unwrap()["internal"], true);
    }

    #[test]
    fn pyrunner_runs_user_code_so_its_activity_is_not_internal() {
        let mut guest = Guest::new();
        guest.calibrate();

        guest.exec(
            SHELL,
            "/usr/bin/python3.real",
            &["/usr/bin/python3.real", "/usr/lib/vpod/pyrunner.py"],
        );
        guest.call(SHELL, open("/data/results.csv"), 3);

        assert!(
            guest
                .events()
                .iter()
                .all(|event| event.get("internal").is_none())
        );
    }

    #[test]
    fn opening_a_vpod_device_or_staging_file_is_internal_but_the_console_is_not() {
        let mut guest = Guest::new();
        guest.calibrate();

        guest.call(SHELL, open("/dev/ttyS1"), 3);
        guest.call(SHELL, open("/tmp/.vpod_cmd.b64"), 3);
        guest.call(SHELL, open("/dev/ttyS0"), 3);

        let events = guest.events();
        assert_eq!(events[0]["internal"], true);
        assert_eq!(events[1]["internal"], true);
        assert!(events[2].get("internal").is_none());
    }

    #[test]
    fn staging_a_long_command_is_internal_but_running_it_is_not() {
        let mut guest = Guest::new();
        guest.calibrate();

        guest.exec(
            SHELL,
            "/bin/base64",
            &["base64", "-d", "/tmp/.vpod_cmd.b64"],
        );
        guest.exec(SHELL, "/bin/rm", &["rm", "-f", "/tmp/.vpod_cmd.b64"]);
        guest.exec(SHELL, "/bin/sh", &["sh", "/tmp/.vpod_cmd.sh"]);
        guest.exec(
            SHELL,
            "/bin/rm",
            &["rm", "-f", "/tmp/.vpod_cmd.b64", "/tmp/notes.txt"],
        );
        guest.exec(SHELL, "/bin/rm", &["rm", "-f"]);

        let events = guest.events();
        assert_eq!(events[0]["internal"], true);
        assert_eq!(events[1]["internal"], true);
        assert!(events[2].get("internal").is_none());
        assert!(events[3].get("internal").is_none());
        assert!(events[4].get("internal").is_none());
    }

    #[test]
    fn an_exit_reports_the_status_the_shell_would_see_and_forgets_the_task() {
        let mut guest = Guest::new();
        guest.calibrate();

        guest.exec(
            SHELL,
            "/usr/lib/vpod/vpod-seed-entropy",
            &["vpod-seed-entropy"],
        );
        guest.syscalls.on_entry(
            entry(Guest::task_of(SHELL), SyscallKind::Exit { code: 256 + 3 }),
            &mut guest.memory,
            0,
        );
        guest.exec(SHELL, "/usr/bin/python3", &["python3"]);

        let events = guest.events();
        assert_eq!(events[1]["kind"], "process.exit");
        assert_eq!(events[1]["code"], 3);
        assert_eq!(events[1]["internal"], true);
        assert!(events[2].get("internal").is_none());
    }

    #[test]
    fn a_relative_path_is_reported_under_the_working_directory_it_was_opened_from() {
        let mut guest = Guest::new();
        guest.calibrate();
        guest.seed(SHELL, "/");
        guest.fork(SHELL, 700);
        guest.call(
            700,
            SyscallKind::ChangeDirectory {
                path: GuestString::Value("/app".into()),
            },
            0,
        );
        guest.call(700, open("src/main.rs"), 3);

        let events = guest.events();
        let open = events.last().unwrap();
        assert_eq!(open["path"], "/app/src/main.rs");
        assert!(open.get("path_unresolved").is_none());
    }

    #[test]
    fn a_child_keeps_resolving_paths_against_the_directory_it_inherited() {
        let mut guest = Guest::new();
        guest.calibrate();
        guest.seed(SHELL, "/app");
        guest.fork(SHELL, 700);
        guest.call(700, open("notes.txt"), 3);

        assert_eq!(guest.events().last().unwrap()["path"], "/app/notes.txt");
    }

    #[test]
    fn a_child_that_runs_before_its_parent_returns_still_knows_where_it_is() {
        let mut guest = Guest::new();
        guest.calibrate();
        guest.seed(SHELL, "/app");
        guest.fork_running_child_first(SHELL, 700, open("notes.txt"), 3);

        let events = guest.events();
        assert_eq!(events[0]["kind"], "file.open");
        assert_eq!(events[0]["pid"], 700);
        assert_eq!(events[0]["path"], "/app/notes.txt");
    }

    #[test]
    fn a_path_that_cannot_be_placed_is_reported_as_the_program_gave_it() {
        let mut guest = Guest::new();
        guest.calibrate();
        guest.call(SHELL, open("notes.txt"), 3);

        let open = guest.events()[0].clone();
        assert_eq!(open["path"], "notes.txt");
        assert_eq!(open["path_unresolved"], true);
    }

    #[test]
    fn a_relative_path_under_a_directory_descriptor_is_placed_by_that_descriptor() {
        let mut guest = Guest::new();
        guest.calibrate();
        guest.seed(SHELL, "/");
        guest.call(SHELL, open("/etc/ssl"), 7);
        guest.call(SHELL, open_at(7, "certs/ca.pem"), 8);

        assert_eq!(
            guest.events().last().unwrap()["path"],
            "/etc/ssl/certs/ca.pem"
        );
    }

    #[test]
    fn truncating_through_a_descriptor_names_the_file_it_points_at() {
        let mut guest = Guest::new();
        guest.calibrate();
        guest.seed(SHELL, "/");
        guest.call(SHELL, open("/tmp/report.csv"), 4);
        guest.call(SHELL, SyscallKind::TruncateDescriptor { fd: 4, size: 0 }, 0);

        let truncate = guest.events().last().unwrap().clone();
        assert_eq!(truncate["kind"], "file.truncate");
        assert_eq!(truncate["path"], "/tmp/report.csv");
        assert_eq!(truncate["size"], 0);
    }

    #[test]
    fn truncating_a_descriptor_that_was_never_seen_opened_says_so() {
        let mut guest = Guest::new();
        guest.calibrate();
        guest.call(
            SHELL,
            SyscallKind::TruncateDescriptor { fd: 9, size: 10 },
            0,
        );

        let truncate = guest.events()[0].clone();
        assert_eq!(truncate["path"], Value::Null);
        assert_eq!(truncate["path_unresolved"], true);
    }

    #[test]
    fn a_closed_descriptor_stops_naming_its_old_file() {
        let mut guest = Guest::new();
        guest.calibrate();
        guest.seed(SHELL, "/");
        guest.call(SHELL, open("/tmp/report.csv"), 4);
        guest.call(SHELL, SyscallKind::Close { fd: 4 }, 0);
        guest.call(SHELL, SyscallKind::TruncateDescriptor { fd: 4, size: 0 }, 0);

        assert_eq!(guest.events().last().unwrap()["path"], Value::Null);
    }

    #[test]
    fn connect_reports_the_protocol_its_socket_was_created_with_and_the_dns_name() {
        let mut guest = Guest::new();
        guest.calibrate();
        guest.tracer.remember_name([151, 101, 0, 223], "pypi.org");

        guest.call(
            SHELL,
            SyscallKind::Socket {
                domain: AF_INET,
                socket_type: SOCK_STREAM | 0o4000, // SOCK_NONBLOCK
            },
            4,
        );
        guest.call(
            SHELL,
            SyscallKind::Connect {
                fd: 4,
                address: Some("151.101.0.223:443".parse().unwrap()),
            },
            -115, // EINPROGRESS
        );

        let events = guest.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "net.connect");
        assert_eq!(events[0]["protocol"], "tcp");
        assert_eq!(events[0]["address"], "151.101.0.223");
        assert_eq!(events[0]["port"], 443);
        assert_eq!(events[0]["host"], "pypi.org");
        assert_eq!(events[0]["result"], -115);
        assert_eq!(events[0]["pid"], 1);
    }

    #[test]
    fn a_connect_on_a_socket_we_never_saw_created_has_no_protocol() {
        let mut guest = Guest::new();
        guest.calibrate();

        guest.call(
            SHELL,
            SyscallKind::Connect {
                fd: 9,
                address: Some("[2a04:4e42::223]:443".parse().unwrap()),
            },
            0,
        );

        let events = guest.events();
        assert_eq!(events[0]["protocol"], Value::Null);
        assert_eq!(events[0]["address"], "2a04:4e42::223");
        assert_eq!(events[0]["host"], Value::Null);
    }

    #[test]
    fn a_unix_socket_connect_is_not_a_network_event() {
        let mut guest = Guest::new();
        guest.calibrate();

        guest.call(
            SHELL,
            SyscallKind::Connect {
                fd: 3,
                address: None,
            },
            0,
        );

        assert!(guest.events().is_empty());
    }

    #[test]
    fn a_listen_is_paired_with_its_bind_address() {
        let mut guest = Guest::new();
        guest.calibrate();

        guest.call(
            SHELL,
            SyscallKind::Socket {
                domain: AF_INET,
                socket_type: SOCK_STREAM,
            },
            5,
        );
        guest.call(
            SHELL,
            SyscallKind::Bind {
                fd: 5,
                address: Some("0.0.0.0:8080".parse().unwrap()),
            },
            0,
        );
        assert!(
            guest.events().is_empty(),
            "socket and bind alone emit nothing"
        );

        guest.call(SHELL, SyscallKind::Listen { fd: 5 }, 0);

        let events = guest.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "net.listen");
        assert_eq!(events[0]["protocol"], "tcp");
        assert_eq!(events[0]["address"], "0.0.0.0");
        assert_eq!(events[0]["port"], 8080);
    }

    #[test]
    fn listen_on_a_socket_that_was_never_seen_bound_emits_nothing() {
        let mut guest = Guest::new();
        guest.calibrate();

        guest.call(SHELL, SyscallKind::Listen { fd: 5 }, 0);

        assert!(guest.events().is_empty());
    }

    #[test]
    fn a_successful_fork_reports_the_child_pid() {
        let mut guest = Guest::new();
        guest.calibrate();

        guest.fork(SHELL, 742);

        let events = guest.events();
        assert_eq!(events[0]["kind"], "process.fork");
        assert_eq!(events[0]["pid"], 1);
        assert_eq!(events[0]["child_pid"], 742);
        assert_eq!(events[0]["thread"], false);
    }

    #[test]
    fn a_ring_the_tracer_cannot_see_through_is_reported_once() {
        let mut guest = Guest::new();
        guest.calibrate();

        guest.call(SHELL, SyscallKind::RingUse, -1); // refused, nothing was hidden
        assert!(guest.events().is_empty());

        guest.call(SHELL, SyscallKind::RingUse, 3);
        guest.call(SHELL, SyscallKind::RingUse, 4);

        let events = guest.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "trace.blind");
        assert_eq!(events[0]["reason"], "io-uring");
        assert_eq!(events[0]["pid"], 1);
    }

    #[test]
    fn a_failed_fork_is_not_reported() {
        let mut guest = Guest::new();
        guest.calibrate();

        guest.call(SHELL, SyscallKind::Clone { thread: false }, -11); // EAGAIN

        assert!(guest.events().is_empty());
    }

    #[test]
    fn a_thread_reports_the_process_it_belongs_to() {
        let mut guest = Guest::new();
        guest.calibrate();
        guest.spawn(900, 900, SHELL);
        guest.call(900, open("/tmp/from-main-thread"), 3);

        guest.spawn_thread(901, 900);
        guest.call_on_task(Guest::task_of(901), open("/tmp/from-worker"), 4);

        let events = guest.events();
        assert_eq!(events[0]["pid"], 900);
        assert_eq!(events[1]["pid"], 900);
        assert_ne!(events[0]["task"], events[1]["task"]);
    }

    #[test]
    fn quiet_keeps_the_state_but_records_nothing() {
        let mut guest = Guest::new();
        guest.syscalls.set_quiet(true);
        guest.calibrate();
        guest.seed(SHELL, "/app");
        guest.call(SHELL, open("hidden.txt"), 3);
        assert!(guest.events().is_empty());

        guest.syscalls.set_quiet(false);
        guest.call(SHELL, open("shown.txt"), 3);

        let events = guest.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["path"], "/app/shown.txt");
    }

    #[test]
    fn disabling_files_still_traces_network() {
        let mut guest = Guest::with_options(TraceOptions {
            files: false,
            ..TraceOptions::default()
        });
        guest.calibrate();

        guest.call(
            SHELL,
            SyscallKind::Mkdir {
                directory_fd: AT_FDCWD,
                path: GuestString::Value("/tmp/x".into()),
            },
            0,
        );
        assert!(guest.events().is_empty());

        guest.call(
            SHELL,
            SyscallKind::Connect {
                fd: 4,
                address: Some("10.0.2.2:443".parse().unwrap()),
            },
            0,
        );
        assert_eq!(guest.events()[0]["kind"], "net.connect");
    }

    #[test]
    fn an_unreadable_path_is_null_and_flagged() {
        let mut guest = Guest::new();
        guest.calibrate();

        guest.call(
            SHELL,
            SyscallKind::Rename {
                from_directory_fd: AT_FDCWD,
                from: GuestString::Truncated("/tmp/aaaa".into()),
                to_directory_fd: AT_FDCWD,
                to: GuestString::Unreadable,
            },
            -14, // EFAULT
        );

        let events = guest.events();
        assert_eq!(events[0]["from"], "/tmp/aaaa");
        assert_eq!(events[0]["from_truncated"], true);
        assert_eq!(events[0]["to"], Value::Null);
        assert_eq!(events[0]["to_unreadable"], true);
    }
}
