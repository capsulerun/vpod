mod logger;
mod run_interactive;
mod vm;

use std::path::PathBuf;

fn usage() -> ! {
    eprintln!("usage: vpod-wasi-cli --snapshot-load <file> [--disk <img>]");
    std::process::exit(1);
}

fn main() {
    log::set_logger(&logger::WasiLogger).ok();
    log::set_max_level(log::LevelFilter::Warn);

    let mut args = std::env::args().skip(1).peekable();

    let mut disk_path: Option<PathBuf> = None;
    let mut snap_load: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--snapshot-load" => snap_load = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--disk" => disk_path = Some(args.next().unwrap_or_else(|| usage()).into()),
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

    let (mut bus, mut hart) = vm::load(vm::VmConfig {
        snapshot: &snap,
        disk: disk_path.as_deref(),
        capture_tx: true,
    })
    .unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });

    run_interactive::run(&mut bus, &mut hart);
}
