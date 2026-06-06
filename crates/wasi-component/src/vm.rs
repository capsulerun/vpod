use flate2::read::GzDecoder;
use machine::machine_bus::MachineBus;
use machine::snapshot;
use riscv_core::Hart;
use std::io::BufReader;
use std::path::Path;

pub struct VmConfig<'a> {
    pub snapshot: &'a Path,
    pub disk: Option<&'a Path>,
    pub capture_tx: bool,
}

pub fn load(config: VmConfig) -> Result<(MachineBus, Hart), String> {
    let mut bus = MachineBus::new(256 * 1024 * 1024);
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
