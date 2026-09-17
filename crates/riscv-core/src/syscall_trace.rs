use crate::execute::ExecContext;
use crate::system_bus::SystemBus;

const SYS_UNLINKAT: u64 = 35;
const SYS_MKDIRAT: u64 = 34;
const SYS_RENAMEAT: u64 = 38;
const SYS_TRUNCATE: u64 = 45;
const SYS_OPENAT: u64 = 56;
const SYS_CLONE: u64 = 220;
const SYS_EXECVE: u64 = 221;
const SYS_BIND: u64 = 200;
const SYS_LISTEN: u64 = 201;
const SYS_CONNECT: u64 = 203;
const SYS_EXIT_GROUP: u64 = 94;
const SYS_RENAMEAT2: u64 = 276;
const SYS_EXECVEAT: u64 = 281;
const SYS_OPENAT2: u64 = 437;
const SYS_CLONE3: u64 = 435;

const CLONE_THREAD: u64 = 0x0001_0000;
const AT_REMOVEDIR: u64 = 0x200;
const AF_INET: u16 = 2;

const MAX_STRING_BYTES: usize = 256;
const MAX_ARGV_ENTRIES: usize = 64;

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
        path: GuestString,
        argv: Vec<GuestString>,
    },
    Exit {
        code: i32,
    },
    Clone {
        thread: bool,
    },
    Open {
        path: GuestString,
        write: bool,
        read_write: bool,
        create: bool,
        truncate: bool,
    },
    Rename {
        from: GuestString,
        to: GuestString,
    },
    Unlink {
        path: GuestString,
        directory: bool,
    },
    Mkdir {
        path: GuestString,
    },
    Truncate {
        path: GuestString,
        size: u64,
    },
    Connect {
        fd: u64,
        address: Option<[u8; 4]>,
        port: u16,
    },
    Bind {
        fd: u64,
        address: Option<[u8; 4]>,
        port: u16,
    },
    Listen {
        fd: u64,
    },
}

pub fn decode_entry<B: SystemBus>(ctx: &mut ExecContext<B>, pc: u64) -> Option<SyscallEntry> {
    let task = ctx.csr.sscratch;
    let number = ctx.regs.read(17);
    let satp = crate::block::effective_satp(*ctx.priv_mode, ctx.csr.satp);
    let args: [u64; 6] = std::array::from_fn(|n| ctx.regs.read(10 + n)); // a0..a5

    let kind = match number {
        SYS_EXECVE | SYS_EXECVEAT => {
            let path_arg = if number == SYS_EXECVEAT {
                args[1]
            } else {
                args[0]
            };
            let argv_arg = if number == SYS_EXECVEAT {
                args[2]
            } else {
                args[1]
            };
            SyscallKind::Exec {
                path: read_cstring(ctx, satp, path_arg),
                argv: read_argv(ctx, satp, argv_arg),
            }
        }
        SYS_EXIT_GROUP => SyscallKind::Exit {
            code: args[0] as i32,
        },
        SYS_CLONE => SyscallKind::Clone {
            thread: args[0] & CLONE_THREAD != 0,
        },
        SYS_CLONE3 => {
            let flags = read_u64(ctx, satp, args[0]).unwrap_or(0);
            SyscallKind::Clone {
                thread: flags & CLONE_THREAD != 0,
            }
        }
        SYS_OPENAT => {
            let flags = args[2] as u32;
            SyscallKind::Open {
                path: read_cstring(ctx, satp, args[1]),
                write: flags & O_ACCMODE == O_WRONLY,
                read_write: flags & O_ACCMODE == O_RDWR,
                create: flags & O_CREAT != 0,
                truncate: flags & O_TRUNC != 0,
            }
        }
        SYS_OPENAT2 => {
            let how_flags = read_u64(ctx, satp, args[2]).unwrap_or(0) as u32;
            SyscallKind::Open {
                path: read_cstring(ctx, satp, args[1]),
                write: how_flags & O_ACCMODE == O_WRONLY,
                read_write: how_flags & O_ACCMODE == O_RDWR,
                create: how_flags & O_CREAT != 0,
                truncate: how_flags & O_TRUNC != 0,
            }
        }
        SYS_RENAMEAT | SYS_RENAMEAT2 => SyscallKind::Rename {
            from: read_cstring(ctx, satp, args[1]),
            to: read_cstring(ctx, satp, args[3]),
        },
        SYS_UNLINKAT => SyscallKind::Unlink {
            path: read_cstring(ctx, satp, args[1]),
            directory: args[2] & AT_REMOVEDIR != 0,
        },
        SYS_MKDIRAT => SyscallKind::Mkdir {
            path: read_cstring(ctx, satp, args[1]),
        },
        SYS_TRUNCATE => SyscallKind::Truncate {
            path: read_cstring(ctx, satp, args[0]),
            size: args[1],
        },
        SYS_CONNECT => {
            let (address, port) = read_sockaddr_in(ctx, satp, args[1]);
            SyscallKind::Connect {
                fd: args[0],
                address,
                port,
            }
        }
        SYS_BIND => {
            let (address, port) = read_sockaddr_in(ctx, satp, args[1]);
            SyscallKind::Bind {
                fd: args[0],
                address,
                port,
            }
        }
        SYS_LISTEN => SyscallKind::Listen { fd: args[0] },
        _ => return None,
    };

    Some(SyscallEntry {
        task,
        number,
        pc,
        kind,
    })
}

const O_ACCMODE: u32 = 0o3;
const O_WRONLY: u32 = 0o1;
const O_RDWR: u32 = 0o2;
const O_CREAT: u32 = 0o100;
const O_TRUNC: u32 = 0o1000;

fn read_u64<B: SystemBus>(ctx: &mut ExecContext<B>, satp: u64, va: u64) -> Option<u64> {
    let pa = ctx.mmu.translate_load(va, satp, ctx.bus).ok()?;
    Some(ctx.bus.read_doubleword(pa))
}

fn read_cstring<B: SystemBus>(ctx: &mut ExecContext<B>, satp: u64, va: u64) -> GuestString {
    if va == 0 {
        return GuestString::Unreadable;
    }
    let mut bytes = Vec::new();
    let mut cursor = va;

    loop {
        if bytes.len() >= MAX_STRING_BYTES {
            return GuestString::Truncated(String::from_utf8_lossy(&bytes).into_owned());
        }

        let Ok(pa) = ctx.mmu.translate_load(cursor, satp, ctx.bus) else {
            return if bytes.is_empty() {
                GuestString::Unreadable
            } else {
                GuestString::Truncated(String::from_utf8_lossy(&bytes).into_owned())
            };
        };

        let byte = ctx.bus.read_byte(pa);
        if byte == 0 {
            return GuestString::Value(String::from_utf8_lossy(&bytes).into_owned());
        }
        bytes.push(byte);
        cursor = cursor.wrapping_add(1);
    }
}

fn read_argv<B: SystemBus>(ctx: &mut ExecContext<B>, satp: u64, mut va: u64) -> Vec<GuestString> {
    if va == 0 {
        return Vec::new();
    }

    let mut argv = Vec::new();
    for _ in 0..MAX_ARGV_ENTRIES {
        let Some(pointer) = read_u64(ctx, satp, va) else {
            break;
        };
        if pointer == 0 {
            break;
        }
        argv.push(read_cstring(ctx, satp, pointer));
        va = va.wrapping_add(8);
    }
    argv
}

fn read_sockaddr_in<B: SystemBus>(
    ctx: &mut ExecContext<B>,
    satp: u64,
    va: u64,
) -> (Option<[u8; 4]>, u16) {
    if va == 0 {
        return (None, 0);
    }

    let Ok(pa) = ctx.mmu.translate_load(va, satp, ctx.bus) else {
        return (None, 0);
    };

    let family = ctx.bus.read_halfword(pa);
    if family != AF_INET {
        return (None, 0);
    }

    let port = u16::from_be_bytes([ctx.bus.read_byte(pa + 2), ctx.bus.read_byte(pa + 3)]);
    let address = [
        ctx.bus.read_byte(pa + 4),
        ctx.bus.read_byte(pa + 5),
        ctx.bus.read_byte(pa + 6),
        ctx.bus.read_byte(pa + 7),
    ];

    (Some(address), port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csr::PrivMode;
    use crate::hart::Hart;
    use crate::system_bus::FlatMemory;

    const ECALL: u32 = 0x0000_0073;
    const SRET: u32 = 0x1020_0073;

    struct RecordingBus {
        memory: FlatMemory,
        entries: Vec<(u64, SyscallKind)>,
        returns: Vec<(u64, u64, i64)>,
    }

    impl RecordingBus {
        fn new() -> Self {
            Self {
                memory: FlatMemory::new(1024 * 1024),
                entries: Vec::new(),
                returns: Vec::new(),
            }
        }

        fn write_cstring(&mut self, address: u64, text: &str) {
            self.memory.load_at(address as usize, text.as_bytes());
            self.memory.load_at(address as usize + text.len(), &[0]);
        }
    }

    impl SystemBus for RecordingBus {
        fn read_byte(&mut self, address: u64) -> u8 {
            self.memory.read_byte(address)
        }
        fn read_halfword(&mut self, address: u64) -> u16 {
            self.memory.read_halfword(address)
        }
        fn read_word(&mut self, address: u64) -> u32 {
            self.memory.read_word(address)
        }
        fn read_doubleword(&mut self, address: u64) -> u64 {
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

        fn syscall_trace_enabled(&self) -> bool {
            true
        }

        fn on_syscall_entry(&mut self, entry: SyscallEntry) {
            self.entries.push((entry.number, entry.kind));
        }

        fn on_syscall_return(&mut self, task: u64, return_pc: u64, value: i64) {
            self.returns.push((task, return_pc, value));
        }
    }

    fn user_ecall(setup: impl FnOnce(&mut Hart, &mut RecordingBus)) -> RecordingBus {
        let mut bus = RecordingBus::new();
        bus.memory.load_at(0, &ECALL.to_le_bytes());

        let mut cpu = Hart::new(0);
        cpu.priv_mode = PrivMode::U;
        cpu.csr.stvec = 0x8000; // somewhere with no instructions; we stop before fetching there
        setup(&mut cpu, &mut bus);

        cpu.run(&mut bus, 1);
        bus
    }

    #[test]
    fn openat_is_decoded_at_entry_with_its_path_and_access_mode() {
        const O_WRONLY: u64 = 0o1;
        const O_CREAT: u64 = 0o100;
        const O_TRUNC: u64 = 0o1000;

        let bus = user_ecall(|cpu, bus| {
            bus.write_cstring(0x2000, "/tmp/trace-demo.txt");
            cpu.regs.write(17, SYS_OPENAT); // a7
            cpu.regs.write(10, u64::MAX); // a0: dirfd, unused by the decoder
            cpu.regs.write(11, 0x2000); // a1: path
            cpu.regs.write(12, O_WRONLY | O_CREAT | O_TRUNC); // a2: flags
        });

        assert_eq!(bus.entries.len(), 1);
        let (number, kind) = &bus.entries[0];
        assert_eq!(*number, SYS_OPENAT);
        match kind {
            SyscallKind::Open {
                path,
                write,
                create,
                truncate,
                ..
            } => {
                assert_eq!(path, &GuestString::Value("/tmp/trace-demo.txt".to_string()));
                assert!(write);
                assert!(create);
                assert!(truncate);
            }
            other => panic!(
                "expected Open, got a different kind: {other:?}",
                other = std::mem::discriminant(other)
            ),
        }
    }

    #[test]
    fn execve_reads_the_path_and_the_whole_argv_array() {
        let bus = user_ecall(|cpu, bus| {
            bus.write_cstring(0x3000, "/usr/bin/wget");
            bus.write_cstring(0x3100, "wget");
            bus.write_cstring(0x3110, "-q");

            bus.memory.load_at(0x3200, &0x3100u64.to_le_bytes()); // argv[0] = "wget"
            bus.memory.load_at(0x3208, &0x3110u64.to_le_bytes()); // argv[1] = "-q"
            bus.memory.load_at(0x3210, &0u64.to_le_bytes()); // argv[2] = NULL

            cpu.regs.write(17, SYS_EXECVE);
            cpu.regs.write(10, 0x3000); // a0: path
            cpu.regs.write(11, 0x3200); // a1: argv
        });

        assert_eq!(bus.entries.len(), 1);
        match &bus.entries[0].1 {
            SyscallKind::Exec { path, argv } => {
                assert_eq!(path, &GuestString::Value("/usr/bin/wget".to_string()));
                assert_eq!(
                    argv,
                    &vec![
                        GuestString::Value("wget".to_string()),
                        GuestString::Value("-q".to_string()),
                    ]
                );
            }
            _ => panic!("expected Exec"),
        }
    }

    #[test]
    fn a_string_longer_than_the_cap_comes_back_truncated() {
        let bus = user_ecall(|cpu, bus| {
            let long_name = "a".repeat(MAX_STRING_BYTES + 50);
            bus.write_cstring(0x4000, &long_name);

            cpu.regs.write(17, SYS_MKDIRAT);
            cpu.regs.write(11, 0x4000); // a1: path
        });

        match &bus.entries[0].1 {
            SyscallKind::Mkdir { path } => match path {
                GuestString::Truncated(text) => assert_eq!(text.len(), MAX_STRING_BYTES),
                other => panic!("expected Truncated, got {other:?}"),
            },
            _ => panic!("expected Mkdir"),
        }
    }

    #[test]
    fn a_null_path_pointer_is_unreadable_not_a_guest_fault() {
        let bus = user_ecall(|cpu, _bus| {
            cpu.regs.write(17, SYS_MKDIRAT);
            cpu.regs.write(11, 0); // a1: path, NULL
        });

        match &bus.entries[0].1 {
            SyscallKind::Mkdir { path } => assert_eq!(path, &GuestString::Unreadable),
            _ => panic!("expected Mkdir"),
        }
    }

    #[test]
    fn connect_decodes_an_ipv4_sockaddr() {
        let bus = user_ecall(|cpu, bus| {
            bus.memory.write_halfword(0x5000, 2); // AF_INET
            bus.memory.write_byte(0x5002, 0x01); // port 0x0150 = 336, big-endian
            bus.memory.write_byte(0x5003, 0x50);
            bus.memory.write_byte(0x5004, 151);
            bus.memory.write_byte(0x5005, 101);
            bus.memory.write_byte(0x5006, 0);
            bus.memory.write_byte(0x5007, 223);

            cpu.regs.write(17, SYS_CONNECT);
            cpu.regs.write(10, 7); // a0: fd
            cpu.regs.write(11, 0x5000); // a1: sockaddr
        });

        match &bus.entries[0].1 {
            SyscallKind::Connect { fd, address, port } => {
                assert_eq!(*fd, 7);
                assert_eq!(*address, Some([151, 101, 0, 223]));
                assert_eq!(*port, 336);
            }
            _ => panic!("expected Connect"),
        }
    }

    #[test]
    fn an_untraced_syscall_number_is_not_decoded_at_all() {
        let bus = user_ecall(|cpu, _bus| {
            cpu.regs.write(17, 64); // sys_write, not one we trace
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
        cpu.regs.write(16, 0);

        cpu.run(&mut bus, 1);

        assert!(bus.entries.is_empty());
    }

    #[test]
    fn sret_to_user_mode_reports_the_task_pc_and_return_value() {
        let mut bus = RecordingBus::new();
        bus.memory.load_at(0, &SRET.to_le_bytes());

        let mut cpu = Hart::new(0);
        cpu.priv_mode = PrivMode::S;
        cpu.csr.sscratch = 0x88_e600;
        cpu.csr.sepc = 0x1000;
        cpu.regs.write(10, 3);

        cpu.run(&mut bus, 1);

        assert_eq!(bus.returns, vec![(0x88_e600, 0x1000, 3)]);
        assert_eq!(cpu.regs.pc, 0x1000);
        assert_eq!(cpu.priv_mode, PrivMode::U);
    }

    #[test]
    fn sret_back_to_supervisor_mode_is_not_reported_as_a_syscall_return() {
        let mut bus = RecordingBus::new();
        bus.memory.load_at(0, &SRET.to_le_bytes());

        let mut cpu = Hart::new(0);
        cpu.priv_mode = PrivMode::S;
        cpu.csr.mstatus |= 1 << 8; // SPP = 1: sret goes back to S, not U
        cpu.csr.sscratch = 0x1234;

        cpu.run(&mut bus, 1);

        assert!(bus.returns.is_empty());
    }
}
