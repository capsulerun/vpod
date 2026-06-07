use flate2::read::GzDecoder;
use machine::machine_bus::MachineBus;
use machine::snapshot;
use riscv_core::Hart;
use std::io::{BufReader, Read, Write};
use std::path::Path;

pub struct VmConfig<'a> {
    pub snapshot: &'a Path,
    pub disk: Option<&'a Path>,
    pub capture_tx: bool,
}

fn uncompressed_cache_path(snapshot_path: &Path) -> std::path::PathBuf {
    let mut cached = snapshot_path.to_path_buf();
    cached.set_extension("raw");
    cached
}

fn ensure_uncompressed(snapshot_path: &Path) -> Result<std::path::PathBuf, String> {
    let cached = uncompressed_cache_path(snapshot_path);

    if cached.exists() {
        return Ok(cached);
    }

    let file = std::fs::File::open(snapshot_path)
        .map_err(|e| format!("failed to open snapshot {:?}: {e}", snapshot_path))?;

    let mut reader = BufReader::new(GzDecoder::new(file));
    let mut data = Vec::new();
    reader
        .read_to_end(&mut data)
        .map_err(|e| format!("failed to decompress snapshot: {e}"))?;

    let tmp = cached.with_extension("raw.tmp");
    let mut out = std::fs::File::create(&tmp)
        .map_err(|e| format!("failed to create cache file: {e}"))?;
    out.write_all(&data)
        .map_err(|e| format!("failed to write cache file: {e}"))?;

    std::fs::rename(&tmp, &cached)
        .map_err(|e| format!("failed to rename cache file: {e}"))?;

    Ok(cached)
}

fn peek_ram_size_raw(path: &Path) -> Result<u64, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("failed to open {:?}: {e}", path))?;

    let mut reader = BufReader::new(file);

    let mut header = [0u8; 16];
    reader
        .read_exact(&mut header)
        .map_err(|e| format!("failed to read snapshot header: {e}"))?;

    if &header[..7] != b"CAPSULE" {
        return Err("invalid snapshot magic".to_string());
    }

    Ok(u64::from_le_bytes(header[8..16].try_into().unwrap()))
}

pub fn load(config: VmConfig) -> Result<(MachineBus, Hart), String> {
    let raw_path = ensure_uncompressed(config.snapshot)?;
    let stored_ram_size = peek_ram_size_raw(&raw_path)?;
    let ram_size = stored_ram_size - 8;

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

    let raw_file = std::fs::File::open(&raw_path)
        .map_err(|e| format!("failed to open raw snapshot: {e}"))?;

    snapshot::restore(&mut bus, &mut hart, &mut BufReader::new(raw_file))
        .map_err(|e| format!("failed to restore snapshot: {e}"))?;

    bus.uart.capture_tx.set(config.capture_tx);

    Ok((bus, hart))
}
