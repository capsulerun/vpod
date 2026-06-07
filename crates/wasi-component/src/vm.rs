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

fn peek_ram_size(snapshot_path: &Path) -> Result<u64, String> {
    let file = std::fs::File::open(snapshot_path)
        .map_err(|e| format!("failed to open snapshot {:?}: {e}", snapshot_path))?;

    let mut reader = BufReader::new(GzDecoder::new(file));

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
    let stored_ram_size = peek_ram_size(config.snapshot)?;
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

    let snapshot_file = std::fs::File::open(config.snapshot)
        .map_err(|e| format!("failed to open snapshot {:?}: {e}", config.snapshot))?;

    snapshot::restore(
        &mut bus,
        &mut hart,
        &mut BufReader::new(GzDecoder::new(snapshot_file)),
    )
    .map_err(|e| format!("failed to restore snapshot: {e}"))?;

    bus.uart.capture_tx.set(config.capture_tx);

    Ok((bus, hart))
}
