mod run_interactive;
mod run_setup;
mod terminal;

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;

use lz4_flex::frame::FrameEncoder;

use machine::machine_bus::{MachineBus, boot};
use machine::snapshot;
use riscv_core::Hart;

fn usage() -> ! {
    eprintln!(
        "usage: vpod <kernel> [--bios <fw>] [--initrd <rd>] [--disk <img>] [--net] [--agent] \
         [--setup <cmds...>] [--ram <mb>] [--bootargs <args>] \
         [--snapshot-save <file>] [--snapshot-load <file>]"
    );
    std::process::exit(1);
}

fn read_file(path: &PathBuf) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| {
        eprintln!("failed to read {:?}: {e}", path);
        std::process::exit(1);
    })
}

fn save_snapshot(bus: &MachineBus, hart: &Hart, path: &PathBuf, flags: u8) {
    match File::create(path) {
        Ok(f) => {
            let mut w = FrameEncoder::new(f);
            match snapshot::save(bus, hart, &mut w, flags) {
                Ok(()) => {
                    if let Err(e) = w.finish() {
                        eprintln!("\r\n[vpod] snapshot finalize failed: {e}");
                        return;
                    }
                    eprintln!("\r\n[vpod] snapshot saved to {:?}", path);
                }
                Err(e) => eprintln!("\r\n[vpod] snapshot save failed: {e}"),
            }
        }
        Err(e) => eprintln!("\r\n[vpod] cannot create snapshot file {:?}: {e}", path),
    }
}

fn main() {
    env_logger::init();

    let mut args = std::env::args().skip(1).peekable();

    let mut bios_path: Option<PathBuf> = None;
    let mut initrd_path: Option<PathBuf> = None;
    let mut disk_path: Option<PathBuf> = None;
    let mut enable_net = false;
    let mut setup_cmds: Vec<String> = Vec::new();
    let mut snap_save: Option<PathBuf> = None;
    let mut snap_warm = false;
    let mut snap_python = false;
    let mut snap_load: Option<PathBuf> = None;
    let mut ram_mb: u64 = 256;
    let mut bootargs = "root=/dev/ram0 rw console=ttyS0 earlycon".to_string();
    let mut trace_insns: u64 = 0;

    let first = args.next().unwrap_or_else(|| usage());
    let mut kernel_path: Option<PathBuf> = None;

    if first.starts_with("--") {
        match first.as_str() {
            "--snapshot-load" => snap_load = Some(args.next().unwrap_or_else(|| usage()).into()),
            _ => {
                eprintln!("unknown argument: {first}");
                usage();
            }
        }
    } else {
        kernel_path = Some(first.into());
    }

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bios" => bios_path = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--initrd" => initrd_path = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--disk" => disk_path = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--net" => enable_net = true,
            "--setup" => {
                setup_cmds.push(args.next().unwrap_or_else(|| usage()));
            }
            "--bootargs" => bootargs = args.next().unwrap_or_else(|| usage()),
            "--snapshot-save" => snap_save = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--snapshot-warm" => snap_warm = true,
            "--snapshot-python" => {
                snap_warm = true;
                snap_python = true;
            }
            "--snapshot-load" => snap_load = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--ram" => {
                ram_mb = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| usage());
            }
            "--trace" => {
                trace_insns = args.next().and_then(|s| s.parse().ok()).unwrap_or(64);
            }
            _ => {
                eprintln!("unknown argument: {arg}");
                usage();
            }
        }
    }

    let ram_size = ram_mb * 1024 * 1024;
    let mut bus = MachineBus::new(ram_size);
    let mut hart = Hart::new(0x1000);

    if let Some(path) = &disk_path {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap_or_else(|e| {
                eprintln!("failed to open disk {:?}: {e}", path);
                std::process::exit(1);
            });
        bus.attach_blk(file).unwrap_or_else(|e| {
            eprintln!("failed to attach disk: {e}");
            std::process::exit(1);
        });
    }

    if enable_net {
        bus.attach_net();
    }

    bus.attach_fs(vec![]);
    bus.attach_crypto();

    let restored_flags = if let Some(ref snap) = snap_load {
        let mut snap_file = std::fs::File::open(snap).unwrap_or_else(|e| {
            eprintln!("failed to open snapshot {:?}: {e}", snap);
            std::process::exit(1);
        });
        let mut magic = [0u8; 4];

        snap_file.read_exact(&mut magic).unwrap_or_else(|e| {
            eprintln!("failed to read snapshot magic: {e}");
            std::process::exit(1);
        });

        snap_file.seek(SeekFrom::Start(0)).unwrap_or_else(|e| {
            eprintln!("failed to seek snapshot: {e}");
            std::process::exit(1);
        });

        let flags = if magic == [0x04, 0x22, 0x4D, 0x18] {
            snapshot::restore(
                &mut bus,
                &mut hart,
                &mut BufReader::new(lz4_flex::frame::FrameDecoder::new(snap_file)),
            )
        } else {
            snapshot::restore(&mut bus, &mut hart, &mut BufReader::new(snap_file))
        }
        .unwrap_or_else(|e| {
            eprintln!("failed to restore snapshot: {e}");
            std::process::exit(1);
        });

        eprintln!(
            "[vpod] restored from snapshot {:?} | disk {:?}",
            snap, disk_path
        );
        flags
    } else {
        let kpath = kernel_path.as_ref().unwrap_or_else(|| {
            eprintln!("kernel path required (or use --snapshot-load)");
            usage();
        });
        let kernel = read_file(kpath);
        let bios = bios_path.as_ref().map(read_file);
        let initrd = initrd_path.as_ref().map(read_file);

        boot(
            &mut bus,
            &mut hart,
            bios.as_deref(),
            &kernel,
            initrd.as_deref(),
            &bootargs,
        );

        eprintln!(
            "[vpod] booting {:?} | bios {:?} | initrd {:?} | RAM {}MB | disk {:?}",
            kpath, bios_path, initrd_path, ram_mb, disk_path
        );
        0
    };

    let snap_flags = (if snap_warm {
        snapshot::FLAG_SHELL_READY
    } else {
        0
    }) | (if snap_python {
        snapshot::FLAG_PYTHON_READY
    } else {
        0
    });

    if !setup_cmds.is_empty() {
        run_setup::run(
            &mut bus,
            &mut hart,
            &setup_cmds,
            snap_save.as_ref(),
            snap_flags,
        );
    } else {
        run_interactive::run(
            &mut bus,
            &mut hart,
            snap_save.as_ref(),
            trace_insns,
            snap_flags,
            restored_flags,
        );
    }
}
