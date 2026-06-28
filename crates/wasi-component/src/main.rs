mod logger;
mod run_interactive;
mod vm;

use std::path::PathBuf;

fn usage() -> ! {
    eprintln!(
        "usage: vpod-wasi-cli --snapshot-load <file> [--disk <img>] [--mount alias:guest[:rw]]"
    );
    std::process::exit(1);
}

fn main() {
    log::set_logger(&logger::WasiLogger).ok();
    log::set_max_level(log::LevelFilter::Warn);

    let mut args = std::env::args().skip(1).peekable();

    let mut disk_path: Option<PathBuf> = None;
    let mut snap_load: Option<PathBuf> = None;
    let mut mounts: Vec<vm::MountArg> = Vec::new();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--snapshot-load" => snap_load = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--disk" => disk_path = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--mount" => {
                let val = args.next().unwrap_or_else(|| usage());
                mounts.push(vm::MountArg::parse(&val));
            }
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

    let mount_args = mounts.clone();
    let (mut bus, mut hart, flags) = vm::load(vm::VmConfig {
        snapshot: &snap,
        disk: disk_path.as_deref(),
        mounts,
        capture_tx: true,
    })
    .unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });

    if flags & machine::snapshot::FLAG_SHELL_READY != 0 {
        let mut script = String::new();
        for (i, mount) in mount_args.iter().enumerate() {
            script.push_str(&format!(
                "mkdir -p {0} && mount -t virtiofs vfs{1} {0} 2>/dev/null; ",
                mount.guest_path, i
            ));
        }

        script.push_str("stty echo; export PS1='\\w # '; trap - EXIT\n");

        for b in script.bytes() {
            bus.uart.push_rx(b);
        }
    }

    run_interactive::run(&mut bus, &mut hart);
}
