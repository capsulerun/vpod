mod logger;
mod run_interactive;
mod run_worker;

use machine::machine_bus::MachineBus;
use machine::snapshot;

use riscv_core::Hart;
use std::io::BufReader;
use std::path::PathBuf;

use flate2::read::GzDecoder;

fn usage() -> ! {
    eprintln!("usage: capsulev-wasi --snapshot-load <file> [--agent] [--disk <img>]");
    std::process::exit(1);
}

fn main() {
    log::set_logger(&logger::WasiLogger).ok();
    log::set_max_level(log::LevelFilter::Warn);

    let mut args = std::env::args().skip(1).peekable();

    let mut disk_path: Option<PathBuf> = None;
    let mut agent_mode = false;
    let mut snap_load: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--snapshot-load" => snap_load = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--disk" => disk_path = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--agent" => agent_mode = true,
            _ => {
                eprintln!("unknown argument: {arg}");
                usage();
            }
        }
    }

    let snap = snap_load.unwrap_or_else(|| {
        eprintln!("--snapshot-load is required");
        usage();
    });

    let mut bus = MachineBus::new(256 * 1024 * 1024); // 256mb ram
    bus.uart.capture_tx.set(true); // Capture UART output for panic detection
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

    {
        let f = std::fs::File::open(&snap).unwrap_or_else(|e| {
            eprintln!("failed to open snapshot {:?}: {e}", snap);
            std::process::exit(1);
        });

        snapshot::restore(&mut bus, &mut hart, &mut BufReader::new(GzDecoder::new(f)))
            .unwrap_or_else(|e| {
                eprintln!("failed to restore snapshot: {e}");
                std::process::exit(1);
            });
    }

    if agent_mode {
        run_worker::run(&mut bus, &mut hart);
    } else {
        run_interactive::run(&mut bus, &mut hart);
    }
}
