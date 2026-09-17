use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::execute::ExecContext;
use crate::system_bus::SystemBus;

const SYS_DUP: u64 = 23;
const SYS_DUP3: u64 = 24;
const SYS_FCNTL: u64 = 25;
const SYS_MKDIRAT: u64 = 34;
const SYS_UNLINKAT: u64 = 35;
const SYS_TRUNCATE: u64 = 45;
const SYS_FTRUNCATE: u64 = 46;
const SYS_CHDIR: u64 = 49;
const SYS_FCHDIR: u64 = 50;
const SYS_OPENAT: u64 = 56;
const SYS_CLOSE: u64 = 57;
const SYS_EXIT_GROUP: u64 = 94;
const SYS_SET_TID_ADDRESS: u64 = 96;
const SYS_GETPID: u64 = 172;
const SYS_GETTID: u64 = 178;
const SYS_SOCKET: u64 = 198;
const SYS_BIND: u64 = 200;
const SYS_LISTEN: u64 = 201;
const SYS_CONNECT: u64 = 203;
const SYS_CLONE: u64 = 220;
const SYS_EXECVE: u64 = 221;
const SYS_RENAMEAT2: u64 = 276;
const SYS_EXECVEAT: u64 = 281;
const SYS_CLONE3: u64 = 435;
const SYS_CLOSE_RANGE: u64 = 436;
const SYS_OPENAT2: u64 = 437;

const CLONE_THREAD: u64 = 0x0001_0000;
const AT_REMOVEDIR: u64 = 0x200;
const AT_EMPTY_PATH: u64 = 0x1000;
const AF_INET: u16 = 2;
const AF_INET6: u16 = 10;

const O_ACCMODE: u32 = 0o3;
const O_WRONLY: u32 = 0o1;
const O_RDWR: u32 = 0o2;
const O_CREAT: u32 = 0o100;
const O_TRUNC: u32 = 0o1000;
const O_CLOEXEC: u32 = 0o2_000_000;

const F_DUPFD: u64 = 0;
const F_DUPFD_CLOEXEC: u64 = 1030;

const MAX_PATH_BYTES: usize = 4096;
const MAX_ARGV_BYTES: usize = 32 * 1024;
const MAX_ARGV_ENTRIES: usize = 1024;

pub const AT_FDCWD: i32 = -100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestString {
    Value(String),
    Truncated(String),
    Unreadable,
}

pub struct SyscallEntry {
    pub task: u64,
    pub number: u64,
    pub pc: u64,
    pub kind: SyscallKind,
}

pub enum SyscallKind {
    Exec {
        directory_fd: i32,
        path: GuestString,
        argv: Vec<String>,
        argv_truncated: bool,
    },
    Exit {
        code: i32,
    },
    Clone {
        thread: bool,
    },
    Identity {
        group: bool,
    },
    Open {
        directory_fd: i32,
        path: GuestString,
        write: bool,
        read_write: bool,
        create: bool,
        truncate: bool,
        close_on_exec: bool,
    },
    Rename {
        from_directory_fd: i32,
        from: GuestString,
        to_directory_fd: i32,
        to: GuestString,
    },
    Unlink {
        directory_fd: i32,
        path: GuestString,
        directory: bool,
    },
    Mkdir {
        directory_fd: i32,
        path: GuestString,
    },
    Truncate {
        path: GuestString,
        size: u64,
    },
    TruncateDescriptor {
        fd: i32,
        size: u64,
    },
    ChangeDirectory {
        path: GuestString,
    },
    ChangeDirectoryDescriptor {
        fd: i32,
    },
    Duplicate {
        fd: i32,
        close_on_exec: bool,
    },
    DuplicateTo {
        from_fd: i32,
        to_fd: i32,
        close_on_exec: bool,
    },
    Close {
        fd: i32,
    },
    CloseRange {
        first: u32,
        last: u32,
    },
    Socket {
        domain: u32,
        socket_type: u32,
    },
    Connect {
        fd: i32,
        address: Option<SocketAddr>,
    },
    Bind {
        fd: i32,
        address: Option<SocketAddr>,
    },
    Listen {
        fd: i32,
    },
}

pub fn decode_entry<B: SystemBus>(
    ctx: &mut ExecContext<B>,
    pc: u64,
    satp: u64,
) -> Option<SyscallEntry> {
    let task = ctx.csr.sscratch;
    let number = ctx.regs.read(17); // a7
    let args: [u64; 6] = std::array::from_fn(|n| ctx.regs.read(10 + n)); // a0..a5
    let mut memory = GuestMemory::new(satp);

    let kind = match number {
        SYS_EXECVE | SYS_EXECVEAT => {
            let (directory_fd, path_address, argv_address) = if number == SYS_EXECVEAT {
                (args[0] as i32, args[1], args[2])
            } else {
                (AT_FDCWD, args[0], args[1])
            };
            let path = if number == SYS_EXECVEAT && args[4] & AT_EMPTY_PATH != 0 {
                GuestString::Value(String::new())
            } else {
                memory.cstring(ctx.bus, path_address, MAX_PATH_BYTES)
            };
            let (argv, argv_truncated) = memory.argv(ctx.bus, argv_address);
            SyscallKind::Exec {
                directory_fd,
                path,
                argv,
                argv_truncated,
            }
        }
        SYS_EXIT_GROUP => SyscallKind::Exit {
            code: args[0] as i32,
        },
        SYS_CLONE => SyscallKind::Clone {
            thread: args[0] & CLONE_THREAD != 0,
        },
        SYS_CLONE3 => SyscallKind::Clone {
            thread: memory.u64(ctx.bus, args[0]).unwrap_or(0) & CLONE_THREAD != 0,
        },
        SYS_SET_TID_ADDRESS | SYS_GETTID => SyscallKind::Identity { group: false },
        SYS_GETPID => SyscallKind::Identity { group: true },
        SYS_OPENAT | SYS_OPENAT2 => {
            let flags = if number == SYS_OPENAT2 {
                memory.u64(ctx.bus, args[2]).unwrap_or(0) as u32
            } else {
                args[2] as u32
            };
            SyscallKind::Open {
                directory_fd: args[0] as i32,
                path: memory.cstring(ctx.bus, args[1], MAX_PATH_BYTES),
                write: flags & O_ACCMODE == O_WRONLY,
                read_write: flags & O_ACCMODE == O_RDWR,
                create: flags & O_CREAT != 0,
                truncate: flags & O_TRUNC != 0,
                close_on_exec: flags & O_CLOEXEC != 0,
            }
        }
        SYS_RENAMEAT2 => SyscallKind::Rename {
            from_directory_fd: args[0] as i32,
            from: memory.cstring(ctx.bus, args[1], MAX_PATH_BYTES),
            to_directory_fd: args[2] as i32,
            to: memory.cstring(ctx.bus, args[3], MAX_PATH_BYTES),
        },
        SYS_UNLINKAT => SyscallKind::Unlink {
            directory_fd: args[0] as i32,
            path: memory.cstring(ctx.bus, args[1], MAX_PATH_BYTES),
            directory: args[2] & AT_REMOVEDIR != 0,
        },
        SYS_MKDIRAT => SyscallKind::Mkdir {
            directory_fd: args[0] as i32,
            path: memory.cstring(ctx.bus, args[1], MAX_PATH_BYTES),
        },
        SYS_TRUNCATE => SyscallKind::Truncate {
            path: memory.cstring(ctx.bus, args[0], MAX_PATH_BYTES),
            size: args[1],
        },
        SYS_FTRUNCATE => SyscallKind::TruncateDescriptor {
            fd: args[0] as i32,
            size: args[1],
        },
        SYS_CHDIR => SyscallKind::ChangeDirectory {
            path: memory.cstring(ctx.bus, args[0], MAX_PATH_BYTES),
        },
        SYS_FCHDIR => SyscallKind::ChangeDirectoryDescriptor { fd: args[0] as i32 },
        SYS_DUP => SyscallKind::Duplicate {
            fd: args[0] as i32,
            close_on_exec: false,
        },
        SYS_FCNTL if matches!(args[1], F_DUPFD | F_DUPFD_CLOEXEC) => SyscallKind::Duplicate {
            fd: args[0] as i32,
            close_on_exec: args[1] == F_DUPFD_CLOEXEC,
        },
        SYS_DUP3 => SyscallKind::DuplicateTo {
            from_fd: args[0] as i32,
            to_fd: args[1] as i32,
            close_on_exec: args[2] as u32 & O_CLOEXEC != 0,
        },
        SYS_CLOSE => SyscallKind::Close { fd: args[0] as i32 },
        SYS_CLOSE_RANGE => SyscallKind::CloseRange {
            first: args[0] as u32,
            last: args[1] as u32,
        },
        SYS_SOCKET => SyscallKind::Socket {
            domain: args[0] as u32,
            socket_type: args[1] as u32,
        },
        SYS_CONNECT => SyscallKind::Connect {
            fd: args[0] as i32,
            address: memory.socket_address(ctx.bus, args[1]),
        },
        SYS_BIND => SyscallKind::Bind {
            fd: args[0] as i32,
            address: memory.socket_address(ctx.bus, args[1]),
        },
        SYS_LISTEN => SyscallKind::Listen { fd: args[0] as i32 },
        _ => return None,
    };

    Some(SyscallEntry {
        task,
        number,
        pc,
        kind,
    })
}

pub struct GuestMemory {
    satp: u64,
    virtual_page: u64,
    host_page: *const u8,
}

impl GuestMemory {
    pub fn new(satp: u64) -> Self {
        Self {
            satp,
            virtual_page: u64::MAX,
            host_page: std::ptr::null(),
        }
    }

    pub fn byte<B: SystemBus>(&mut self, bus: &mut B, virtual_address: u64) -> Option<u8> {
        let virtual_page = virtual_address >> 12;
        if virtual_page != self.virtual_page {
            let physical_address =
                crate::mmu::Mmu::translate_readable(virtual_address, self.satp, bus)?;
            self.host_page = bus.ram_load_page(physical_address)?;
            self.virtual_page = virtual_page;
        }

        Some(unsafe { *self.host_page.add((virtual_address & 0xfff) as usize) })
    }

    pub fn array<B: SystemBus, const N: usize>(
        &mut self,
        bus: &mut B,
        virtual_address: u64,
    ) -> Option<[u8; N]> {
        let mut bytes = [0u8; N];
        for (offset, byte) in bytes.iter_mut().enumerate() {
            *byte = self.byte(bus, virtual_address.wrapping_add(offset as u64))?;
        }
        Some(bytes)
    }

    pub fn u32<B: SystemBus>(&mut self, bus: &mut B, virtual_address: u64) -> Option<u32> {
        self.array(bus, virtual_address).map(u32::from_le_bytes)
    }

    pub fn u64<B: SystemBus>(&mut self, bus: &mut B, virtual_address: u64) -> Option<u64> {
        self.array(bus, virtual_address).map(u64::from_le_bytes)
    }

    fn cstring<B: SystemBus>(
        &mut self,
        bus: &mut B,
        virtual_address: u64,
        max_bytes: usize,
    ) -> GuestString {
        if virtual_address == 0 {
            return GuestString::Unreadable;
        }

        let mut bytes = Vec::new();
        loop {
            let next = virtual_address.wrapping_add(bytes.len() as u64);
            match self.byte(bus, next) {
                None if bytes.is_empty() => return GuestString::Unreadable,
                None => return GuestString::Truncated(lossy(bytes)),
                Some(0) => return GuestString::Value(lossy(bytes)),
                Some(_) if bytes.len() == max_bytes => {
                    return GuestString::Truncated(lossy(bytes));
                }
                Some(byte) => bytes.push(byte),
            }
        }
    }

    fn argv<B: SystemBus>(&mut self, bus: &mut B, virtual_address: u64) -> (Vec<String>, bool) {
        let mut argv = Vec::new();
        if virtual_address == 0 {
            return (argv, false);
        }

        let mut budget = MAX_ARGV_BYTES;
        for index in 0..MAX_ARGV_ENTRIES as u64 {
            let Some(pointer) = self.u64(bus, virtual_address.wrapping_add(index * 8)) else {
                return (argv, true);
            };
            if pointer == 0 {
                return (argv, false);
            }
            if budget == 0 {
                return (argv, true);
            }

            match self.cstring(bus, pointer, budget) {
                GuestString::Value(argument) => {
                    budget = budget.saturating_sub(argument.len());
                    argv.push(argument);
                }
                GuestString::Truncated(argument) => {
                    argv.push(argument);
                    return (argv, true);
                }
                GuestString::Unreadable => return (argv, true),
            }
        }

        (argv, true)
    }

    fn socket_address<B: SystemBus>(
        &mut self,
        bus: &mut B,
        virtual_address: u64,
    ) -> Option<SocketAddr> {
        if virtual_address == 0 {
            return None;
        }

        let family = u16::from_le_bytes(self.array(bus, virtual_address)?);
        let port = u16::from_be_bytes(self.array(bus, virtual_address + 2)?);

        match family {
            AF_INET => {
                let octets: [u8; 4] = self.array(bus, virtual_address + 4)?;
                Some(SocketAddr::from((Ipv4Addr::from(octets), port)))
            }
            AF_INET6 => {
                let octets: [u8; 16] = self.array(bus, virtual_address + 8)?;
                Some(SocketAddr::from((Ipv6Addr::from(octets), port)))
            }
            _ => None,
        }
    }
}

fn lossy(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csr::PrivMode;
    use crate::hart::Hart;
    use crate::system_bus::FlatMemory;

    const ECALL: u32 = 0x0000_0073;
    const SRET: u32 = 0x1020_0073;
    const DEVICE_BASE: u64 = 0x8_0000;

    struct RecordingBus {
        memory: FlatMemory,
        device_reads: usize,
        entries: Vec<(u64, SyscallKind)>,
        returns: Vec<(u64, u64, i64)>,
    }

    impl RecordingBus {
        fn new() -> Self {
            Self {
                memory: FlatMemory::new(1024 * 1024),
                device_reads: 0,
                entries: Vec::new(),
                returns: Vec::new(),
            }
        }

        fn write_cstring(&mut self, address: u64, text: &str) {
            self.memory.load_at(address as usize, text.as_bytes());
            self.memory.load_at(address as usize + text.len(), &[0]);
        }

        fn write_u64(&mut self, address: u64, value: u64) {
            self.memory.load_at(address as usize, &value.to_le_bytes());
        }

        fn count_device(&mut self, address: u64) {
            if address >= DEVICE_BASE {
                self.device_reads += 1;
            }
        }
    }

    impl SystemBus for RecordingBus {
        fn read_byte(&mut self, address: u64) -> u8 {
            self.count_device(address);
            self.memory.read_byte(address)
        }
        fn read_halfword(&mut self, address: u64) -> u16 {
            self.count_device(address);
            self.memory.read_halfword(address)
        }
        fn read_word(&mut self, address: u64) -> u32 {
            self.count_device(address);
            self.memory.read_word(address)
        }
        fn read_doubleword(&mut self, address: u64) -> u64 {
            self.count_device(address);
            self.memory.read_doubleword(address)
        }
        fn write_byte(&mut self, address: u64, value: u8) {
            self.memory.write_byte(address, value)
        }
        fn write_halfword(&mut self, address: u64, value: u16) {
            self.memory.write_halfword(address, value)
        }
        fn write_word(&mut self, address: u64, value: u32) {
            self.memory.write_word(address, value)
        }
        fn write_doubleword(&mut self, address: u64, value: u64) {
            self.memory.write_doubleword(address, value)
        }

        fn ram_load_page(&mut self, address: u64) -> Option<*const u8> {
            if address >= DEVICE_BASE {
                return None;
            }
            self.memory.ram_load_page(address)
        }

        fn syscall_trace_enabled(&self) -> bool {
            true
        }

        fn on_syscall_entry(&mut self, entry: SyscallEntry, _satp: u64) {
            self.entries.push((entry.number, entry.kind));
        }

        fn on_syscall_return(&mut self, task: u64, return_pc: u64, value: i64, _satp: u64) {
            self.returns.push((task, return_pc, value));
        }
    }

    fn user_ecall(setup: impl FnOnce(&mut Hart, &mut RecordingBus)) -> RecordingBus {
        let mut bus = RecordingBus::new();
        bus.memory.load_at(0, &ECALL.to_le_bytes());

        let mut cpu = Hart::new(0);
        cpu.priv_mode = PrivMode::U;
        setup(&mut cpu, &mut bus);

        cpu.run(&mut bus, 1);
        bus
    }

    fn only_entry(bus: RecordingBus) -> SyscallKind {
        let mut entries = bus.entries;
        assert_eq!(entries.len(), 1, "expected exactly one decoded syscall");
        entries.remove(0).1
    }

    #[test]
    fn openat_is_decoded_at_entry_with_its_path_and_access_mode() {
        let kind = only_entry(user_ecall(|cpu, bus| {
            bus.write_cstring(0x2000, "/tmp/trace-demo.txt");
            cpu.regs.write(17, SYS_OPENAT);
            cpu.regs.write(10, AT_FDCWD as u64); // a0: dirfd
            cpu.regs.write(11, 0x2000); // a1: path
            cpu.regs.write(12, (O_WRONLY | O_CREAT | O_TRUNC) as u64); // a2: flags
        }));

        let SyscallKind::Open {
            directory_fd,
            path,
            write,
            read_write,
            create,
            truncate,
            close_on_exec,
        } = kind
        else {
            panic!("expected Open");
        };
        assert_eq!(directory_fd, AT_FDCWD);
        assert_eq!(path, GuestString::Value("/tmp/trace-demo.txt".into()));
        assert!(write && create && truncate && !read_write && !close_on_exec);
    }

    #[test]
    fn openat_keeps_the_directory_descriptor_a_relative_path_is_read_against() {
        let kind = only_entry(user_ecall(|cpu, bus| {
            bus.write_cstring(0x2000, "package.json");
            cpu.regs.write(17, SYS_OPENAT);
            cpu.regs.write(10, 7);
            cpu.regs.write(11, 0x2000);
            cpu.regs.write(12, O_CLOEXEC as u64);
        }));

        let SyscallKind::Open {
            directory_fd,
            path,
            close_on_exec,
            ..
        } = kind
        else {
            panic!("expected Open");
        };
        assert_eq!(directory_fd, 7);
        assert_eq!(path, GuestString::Value("package.json".into()));
        assert!(close_on_exec);
    }

    #[test]
    fn execve_reads_the_path_and_the_whole_argv_array() {
        let kind = only_entry(user_ecall(|cpu, bus| {
            bus.write_cstring(0x3000, "/bin/sh");
            bus.write_cstring(0x3100, "sh");
            bus.write_cstring(0x3110, "-c");
            bus.write_cstring(0x3120, "cd /app && make");
            bus.write_u64(0x3200, 0x3100);
            bus.write_u64(0x3208, 0x3110);
            bus.write_u64(0x3210, 0x3120);
            bus.write_u64(0x3218, 0);

            cpu.regs.write(17, SYS_EXECVE);
            cpu.regs.write(10, 0x3000); // a0: path
            cpu.regs.write(11, 0x3200); // a1: argv
        }));

        let SyscallKind::Exec {
            directory_fd,
            path,
            argv,
            argv_truncated,
        } = kind
        else {
            panic!("expected Exec");
        };
        assert_eq!(directory_fd, AT_FDCWD);
        assert_eq!(path, GuestString::Value("/bin/sh".into()));
        assert_eq!(argv, ["sh", "-c", "cd /app && make"]);
        assert!(!argv_truncated);
    }

    #[test]
    fn execveat_on_a_descriptor_alone_has_an_empty_path() {
        let kind = only_entry(user_ecall(|cpu, bus| {
            bus.write_u64(0x3200, 0);
            cpu.regs.write(17, SYS_EXECVEAT);
            cpu.regs.write(10, 9); // a0: dirfd
            cpu.regs.write(11, 0); // a1: path
            cpu.regs.write(12, 0x3200); // a2: argv
            cpu.regs.write(14, AT_EMPTY_PATH); // a4: flags
        }));

        let SyscallKind::Exec {
            directory_fd, path, ..
        } = kind
        else {
            panic!("expected Exec");
        };
        assert_eq!(directory_fd, 9);
        assert_eq!(path, GuestString::Value(String::new()));
    }

    #[test]
    fn a_command_line_past_the_argv_budget_is_kept_up_to_it_and_marked() {
        let kind = only_entry(user_ecall(|cpu, bus| {
            bus.write_cstring(0x3000, "/bin/sh");
            bus.write_cstring(0x3100, "sh");
            bus.write_cstring(0x3110, "-c");
            bus.write_cstring(0x4000, &"x".repeat(MAX_ARGV_BYTES + 100));
            bus.write_u64(0x3200, 0x3100);
            bus.write_u64(0x3208, 0x3110);
            bus.write_u64(0x3210, 0x4000);
            bus.write_u64(0x3218, 0);

            cpu.regs.write(17, SYS_EXECVE);
            cpu.regs.write(10, 0x3000);
            cpu.regs.write(11, 0x3200);
        }));

        let SyscallKind::Exec {
            argv,
            argv_truncated,
            ..
        } = kind
        else {
            panic!("expected Exec");
        };
        assert!(argv_truncated);
        assert_eq!(argv.len(), 3);
        assert_eq!(argv.iter().map(String::len).sum::<usize>(), MAX_ARGV_BYTES);
    }

    #[test]
    fn a_path_longer_than_path_max_comes_back_truncated() {
        let kind = only_entry(user_ecall(|cpu, bus| {
            bus.write_cstring(0x4000, &"a".repeat(MAX_PATH_BYTES + 50));
            cpu.regs.write(17, SYS_MKDIRAT);
            cpu.regs.write(11, 0x4000);
        }));

        let SyscallKind::Mkdir { path, .. } = kind else {
            panic!("expected Mkdir");
        };
        let GuestString::Truncated(text) = path else {
            panic!("expected Truncated, got {path:?}");
        };
        assert_eq!(text.len(), MAX_PATH_BYTES);
    }

    #[test]
    fn a_path_exactly_at_path_max_is_whole() {
        let kind = only_entry(user_ecall(|cpu, bus| {
            bus.write_cstring(0x4000, &"a".repeat(MAX_PATH_BYTES));
            cpu.regs.write(17, SYS_MKDIRAT);
            cpu.regs.write(11, 0x4000);
        }));

        let SyscallKind::Mkdir { path, .. } = kind else {
            panic!("expected Mkdir");
        };
        assert!(matches!(path, GuestString::Value(text) if text.len() == MAX_PATH_BYTES));
    }

    #[test]
    fn a_null_path_pointer_is_unreadable() {
        let kind = only_entry(user_ecall(|cpu, _bus| {
            cpu.regs.write(17, SYS_MKDIRAT);
            cpu.regs.write(11, 0);
        }));

        let SyscallKind::Mkdir { path, .. } = kind else {
            panic!("expected Mkdir");
        };
        assert_eq!(path, GuestString::Unreadable);
    }

    #[test]
    fn a_pointer_onto_a_device_is_unreadable_and_never_touches_the_device() {
        let bus = user_ecall(|cpu, _bus| {
            cpu.regs.write(17, SYS_MKDIRAT);
            cpu.regs.write(11, DEVICE_BASE + 0x10);
        });

        assert_eq!(bus.device_reads, 0, "tracing read a device register");
        let SyscallKind::Mkdir { path, .. } = only_entry(bus) else {
            panic!("expected Mkdir");
        };
        assert_eq!(path, GuestString::Unreadable);
    }

    #[test]
    fn chdir_and_fchdir_are_decoded() {
        let kind = only_entry(user_ecall(|cpu, bus| {
            bus.write_cstring(0x2000, "/app");
            cpu.regs.write(17, SYS_CHDIR);
            cpu.regs.write(10, 0x2000);
        }));
        assert!(matches!(kind, SyscallKind::ChangeDirectory { path }
            if path == GuestString::Value("/app".into())));

        let kind = only_entry(user_ecall(|cpu, _bus| {
            cpu.regs.write(17, SYS_FCHDIR);
            cpu.regs.write(10, 5);
        }));
        assert!(matches!(
            kind,
            SyscallKind::ChangeDirectoryDescriptor { fd: 5 }
        ));
    }

    #[test]
    fn descriptor_calls_that_move_paths_between_numbers_are_decoded() {
        let kind = only_entry(user_ecall(|cpu, _bus| {
            cpu.regs.write(17, SYS_DUP3);
            cpu.regs.write(10, 4);
            cpu.regs.write(11, 9);
            cpu.regs.write(12, O_CLOEXEC as u64);
        }));
        assert!(matches!(
            kind,
            SyscallKind::DuplicateTo {
                from_fd: 4,
                to_fd: 9,
                close_on_exec: true
            }
        ));

        let kind = only_entry(user_ecall(|cpu, _bus| {
            cpu.regs.write(17, SYS_FCNTL);
            cpu.regs.write(10, 3);
            cpu.regs.write(11, F_DUPFD_CLOEXEC);
        }));
        assert!(matches!(
            kind,
            SyscallKind::Duplicate {
                fd: 3,
                close_on_exec: true
            }
        ));

        let kind = only_entry(user_ecall(|cpu, _bus| {
            cpu.regs.write(17, SYS_CLOSE_RANGE);
            cpu.regs.write(10, 3);
            cpu.regs.write(11, u32::MAX as u64);
        }));
        assert!(matches!(
            kind,
            SyscallKind::CloseRange {
                first: 3,
                last: u32::MAX
            }
        ));
    }

    #[test]
    fn an_fcntl_that_is_not_a_duplicate_is_not_decoded() {
        let bus = user_ecall(|cpu, _bus| {
            cpu.regs.write(17, SYS_FCNTL);
            cpu.regs.write(10, 3);
            cpu.regs.write(11, 4); // F_SETFL
        });
        assert!(bus.entries.is_empty());
    }

    #[test]
    fn the_calls_that_name_a_task_are_marked_thread_or_process() {
        let kind = only_entry(user_ecall(|cpu, _bus| {
            cpu.regs.write(17, SYS_GETTID);
        }));
        assert!(matches!(kind, SyscallKind::Identity { group: false }));

        let kind = only_entry(user_ecall(|cpu, _bus| {
            cpu.regs.write(17, SYS_GETPID);
        }));
        assert!(matches!(kind, SyscallKind::Identity { group: true }));

        let kind = only_entry(user_ecall(|cpu, _bus| {
            cpu.regs.write(17, SYS_SET_TID_ADDRESS);
        }));
        assert!(matches!(kind, SyscallKind::Identity { group: false }));
    }

    #[test]
    fn connect_decodes_an_ipv4_sockaddr() {
        let kind = only_entry(user_ecall(|cpu, bus| {
            bus.memory.load_at(0x5000, &AF_INET.to_le_bytes());
            bus.memory.load_at(0x5002, &443u16.to_be_bytes());
            bus.memory.load_at(0x5004, &[151, 101, 0, 223]);

            cpu.regs.write(17, SYS_CONNECT);
            cpu.regs.write(10, 7); // a0: fd
            cpu.regs.write(11, 0x5000); // a1: sockaddr
        }));

        let SyscallKind::Connect { fd, address } = kind else {
            panic!("expected Connect");
        };
        assert_eq!(fd, 7);
        assert_eq!(address, Some("151.101.0.223:443".parse().unwrap()));
    }

    #[test]
    fn connect_decodes_an_ipv6_sockaddr() {
        let kind = only_entry(user_ecall(|cpu, bus| {
            bus.memory.load_at(0x5000, &AF_INET6.to_le_bytes());
            bus.memory.load_at(0x5002, &80u16.to_be_bytes());
            bus.memory.load_at(0x5004, &0u32.to_be_bytes()); // flowinfo
            bus.memory.load_at(
                0x5008,
                &"2a04:4e42::223".parse::<Ipv6Addr>().unwrap().octets(),
            );

            cpu.regs.write(17, SYS_CONNECT);
            cpu.regs.write(11, 0x5000);
        }));

        let SyscallKind::Connect { address, .. } = kind else {
            panic!("expected Connect");
        };
        assert_eq!(address, Some("[2a04:4e42::223]:80".parse().unwrap()));
    }

    #[test]
    fn a_unix_socket_connect_has_no_ip_address() {
        const AF_UNIX: u16 = 1;
        let kind = only_entry(user_ecall(|cpu, bus| {
            bus.memory.load_at(0x5000, &AF_UNIX.to_le_bytes());
            bus.write_cstring(0x5002, "/run/vpod-pyd.sock");

            cpu.regs.write(17, SYS_CONNECT);
            cpu.regs.write(11, 0x5000);
        }));

        let SyscallKind::Connect { address, .. } = kind else {
            panic!("expected Connect");
        };
        assert_eq!(address, None);
    }

    #[test]
    fn an_untraced_syscall_number_is_not_decoded_at_all() {
        let bus = user_ecall(|cpu, _bus| {
            cpu.regs.write(17, 64); // write
        });
        assert!(bus.entries.is_empty());
    }

    #[test]
    fn renameat_is_not_part_of_the_riscv64_abi() {
        let bus = user_ecall(|cpu, bus| {
            bus.write_cstring(0x2000, "/tmp/a");
            cpu.regs.write(17, 38);
            cpu.regs.write(11, 0x2000);
        });
        assert!(bus.entries.is_empty());
    }

    #[test]
    fn a_kernel_mode_ecall_is_never_treated_as_a_user_syscall() {
        let mut bus = RecordingBus::new();
        bus.memory.load_at(0, &ECALL.to_le_bytes());

        let mut cpu = Hart::new(0);
        cpu.priv_mode = PrivMode::S;
        cpu.regs.write(17, SYS_OPENAT);

        cpu.run(&mut bus, 1);

        assert!(bus.entries.is_empty());
    }

    #[test]
    fn sret_to_user_mode_reports_the_task_pc_and_return_value() {
        let mut bus = RecordingBus::new();
        bus.memory.load_at(0, &SRET.to_le_bytes());

        let mut cpu = Hart::new(0);
        cpu.priv_mode = PrivMode::S;
        cpu.csr.sscratch = 0xffff_ffd8_0088_e600;
        cpu.csr.sepc = 0x1000;
        cpu.regs.write(10, 3);

        cpu.run(&mut bus, 1);

        assert_eq!(bus.returns, vec![(0xffff_ffd8_0088_e600, 0x1000, 3)]);
        assert_eq!(cpu.regs.pc, 0x1000);
        assert_eq!(cpu.priv_mode, PrivMode::U);
    }

    #[test]
    fn sret_back_to_supervisor_mode_is_not_reported_as_a_syscall_return() {
        let mut bus = RecordingBus::new();
        bus.memory.load_at(0, &SRET.to_le_bytes());

        let mut cpu = Hart::new(0);
        cpu.priv_mode = PrivMode::S;
        cpu.csr.mstatus |= 1 << 8; // SPP = S
        cpu.csr.sscratch = 0x1234;

        cpu.run(&mut bus, 1);

        assert!(bus.returns.is_empty());
    }
}
