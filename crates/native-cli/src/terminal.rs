use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

use machine::machine_bus::MachineBus;
use riscv_core::Hart;

pub struct RawTerminal {
    saved: libc::termios,
}

impl RawTerminal {
    pub fn enter() -> Option<Self> {
        if unsafe { libc::isatty(libc::STDIN_FILENO) } == 0 {
            return None;
        }

        // see if better option
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0 {
                return None;
            }

            let saved = t;
            libc::cfmakeraw(&mut t);

            t.c_oflag |= libc::OPOST;
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &t);
            Some(Self { saved })
        }
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &self.saved);
        }
    }
}

pub fn set_nonblocking() {
    let fd = std::io::stdin().as_raw_fd();

    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL, 0);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
}

pub fn poll_stdin(bus: &mut MachineBus, snap_path: Option<&PathBuf>, hart: &Hart, snap_flags: u8) {
    let mut buf = [0u8; 64];

    match std::io::stdin().read(&mut buf) {
        Ok(n) if n > 0 => {
            for &b in &buf[..n] {
                match b {
                    0x1d | 0x03 => {
                        eprintln!("\r\n[vpod] exiting.");

                        if let Some(report) = riscv_core::perf::report() {
                            eprint!("{}", report.replace('\n', "\r\n"));
                        }

                        std::process::exit(0);
                    }
                    0x13 => {
                        if let Some(path) = snap_path {
                            super::save_snapshot(bus, hart, path, snap_flags);
                        }
                    }
                    0x10 => match riscv_core::perf::report() {
                        Some(report) => eprint!("\r\n{}", report.replace('\n', "\r\n")),
                        None => eprintln!("\r\n[vpod] perf counters empty or disabled"),
                    },
                    _ => bus.uart.push_rx(b),
                }
            }
        }
        _ => {}
    }
}
