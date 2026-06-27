use lz4_flex::frame::FrameDecoder;
use machine::machine_bus::MachineBus;
use machine::snapshot;
use machine::virtio::fs::Mount;
use riscv_core::Hart;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

pub struct MountArg {
    pub alias: String,

    #[allow(dead_code)]
    pub guest_path: String, // clippy doesn't see the usage in main.rs

    pub writable: bool,
}

impl MountArg {
    #[allow(dead_code)] // clippy doesn't see the usage in main.rs
    pub fn parse(s: &str) -> Self {
        let parts: Vec<&str> = s.split(':').collect();

        match parts.len() {
            2 => Self {
                alias: parts[0].to_string(),
                guest_path: parts[1].to_string(),
                writable: false,
            },
            3 => Self {
                alias: parts[0].to_string(),
                guest_path: parts[1].to_string(),
                writable: parts[2] == "rw",
            },
            _ => Self {
                alias: s.to_string(),
                guest_path: "/mnt".to_string(),
                writable: false,
            },
        }
    }
}

pub struct VmConfig<'a> {
    pub snapshot: &'a Path,
    pub disk: Option<&'a Path>,
    pub mounts: Vec<MountArg>,
    pub capture_tx: bool,
}

fn ram_size_from_filename(snapshot_path: &Path) -> Option<u64> {
    let stem = snapshot_path.file_stem()?.to_str()?;
    for part in stem.rsplit('-') {
        let lower = part.to_ascii_lowercase();
        if lower.ends_with("mb") {
            return lower
                .trim_end_matches("mb")
                .parse::<u64>()
                .ok()
                .map(|mb| mb * 1024 * 1024);
        }

        if lower.ends_with("gb") {
            return lower
                .trim_end_matches("gb")
                .parse::<u64>()
                .ok()
                .map(|gb| gb * 1024 * 1024 * 1024);
        }
    }
    None
}

enum Compression {
    Lz4,
    Raw,
}

fn detect_compression(path: &Path) -> Result<Compression, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("failed to open {:?}: {e}", path))?;
    let mut magic = [0u8; 4];
    let n = file
        .read(&mut magic)
        .map_err(|e| format!("failed to read file magic: {e}"))?;
    if n >= 4 && magic == [0x04, 0x22, 0x4D, 0x18] {
        Ok(Compression::Lz4)
    } else {
        Ok(Compression::Raw)
    }
}

pub fn load(config: VmConfig) -> Result<(MachineBus, Hart, u8), String> {
    let ram_size = ram_size_from_filename(config.snapshot).unwrap_or(256 * 1024 * 1024);

    let mut bus = MachineBus::new(ram_size);
    bus.attach_net();
    let mut hart = Hart::new(0x1000);

    if let Some(disk_path) = config.disk {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(disk_path)
            .map_err(|e| format!("failed to open disk {disk_path:?}: {e}"))?;

        bus.attach_blk(file)
            .map_err(|e| format!("failed to attach disk: {e}"))?;
    }

    let snapshot_file = std::fs::File::open(config.snapshot)
        .map_err(|e| format!("failed to open snapshot {:?}: {e}", config.snapshot))?;

    let flags = match detect_compression(config.snapshot)? {
        Compression::Lz4 => snapshot::restore(
            &mut bus,
            &mut hart,
            &mut BufReader::new(FrameDecoder::new(snapshot_file)),
        ),
        Compression::Raw => {
            snapshot::restore(&mut bus, &mut hart, &mut BufReader::new(snapshot_file))
        }
    }
    .map_err(|e| format!("failed to restore snapshot: {e}"))?;

    if !config.mounts.is_empty() {
        let mounts: Vec<Mount> = config
            .mounts
            .iter()
            .map(|m| Mount {
                host_path: PathBuf::from(&m.alias),
                tag: "virtiofs".to_string(),
                writable: m.writable,
            })
            .collect();
        bus.attach_fs(mounts);
    }

    bus.uart.capture_tx.set(config.capture_tx);
    bus.uart_stderr.capture_tx.set(true);
    bus.uart_ctrl.capture_tx.set(true);
    bus.uart_data.capture_tx.set(true);

    Ok((bus, hart, flags))
}
