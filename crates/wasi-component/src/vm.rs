use flate2::read::GzDecoder;
use machine::machine_bus::MachineBus;
use machine::snapshot;
use riscv_core::Hart;
use std::io::{BufReader, Read};
use std::path::Path;

pub struct VmConfig<'a> {
    pub snapshot: &'a Path,
    pub disk: Option<&'a Path>,
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

fn is_gzipped(path: &Path) -> Result<bool, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("failed to open {:?}: {e}", path))?;
    let mut magic = [0u8; 2];
    file.read_exact(&mut magic)
        .map_err(|e| format!("failed to read file magic: {e}"))?;
    Ok(magic == [0x1f, 0x8b])
}

pub fn load(config: VmConfig) -> Result<(MachineBus, Hart), String> {
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

    if is_gzipped(config.snapshot)? {
        snapshot::restore(
            &mut bus,
            &mut hart,
            &mut BufReader::new(GzDecoder::new(snapshot_file)),
        )
        .map_err(|e| format!("failed to restore snapshot: {e}"))?;
    } else {
        snapshot::restore(&mut bus, &mut hart, &mut BufReader::new(snapshot_file))
            .map_err(|e| format!("failed to restore snapshot: {e}"))?;
    }

    bus.uart.capture_tx.set(config.capture_tx);

    Ok((bus, hart))
}
