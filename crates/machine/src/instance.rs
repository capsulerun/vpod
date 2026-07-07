use std::fs;
use std::io;
use std::path::PathBuf;

use riscv_core::Hart;

use crate::machine_bus::MachineBus;
use crate::snapshot;

const COLLAPSE_THRESHOLD: f64 = 0.25;

pub fn instances_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".vpod")
        .join("instances")
}

pub fn instance_dir(instance_id: &str) -> PathBuf {
    instances_dir().join(instance_id)
}

pub fn suspend(bus: &MachineBus, hart: &Hart, instance_id: &str) -> io::Result<()> {
    let dir = instance_dir(instance_id);
    fs::create_dir_all(&dir)?;

    let mut delta_buf = Vec::new();
    snapshot::save_delta(bus, hart, &mut delta_buf)?;
    fs::write(dir.join("delta.bin"), &delta_buf)?;

    update_manifest(instance_id, "SUSPENDED")?;
    Ok(())
}

pub fn resume(bus: &mut MachineBus, hart: &mut Hart, instance_id: &str) -> io::Result<()> {
    let dir = instance_dir(instance_id);
    let delta_bytes = fs::read(dir.join("delta.bin"))?;

    let mut cursor = io::Cursor::new(&delta_bytes);
    snapshot::restore_delta(bus, hart, &mut cursor)?;

    update_manifest(instance_id, "RUNNING")?;
    Ok(())
}

pub fn should_collapse(bus: &MachineBus) -> bool {
    let dirty_count = bus.dirty_pages().len();
    let total_pages = (bus.ram_size() >> 12) as usize;
    dirty_count as f64 / total_pages as f64 > COLLAPSE_THRESHOLD
}

pub fn collapse(bus: &MachineBus, hart: &Hart, instance_id: &str, flags: u8) -> io::Result<()> {
    let dir = instance_dir(instance_id);
    let base_path = dir.join("base.vpod");

    let mut buf = Vec::new();
    snapshot::save(bus, hart, &mut buf, flags)?;
    fs::write(&base_path, &buf)?;

    let delta_path = dir.join("delta.bin");
    if delta_path.exists() {
        fs::remove_file(&delta_path)?;
    }

    Ok(())
}

pub fn destroy(instance_id: &str) -> io::Result<()> {
    let dir = instance_dir(instance_id);
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    remove_from_manifest(instance_id)?;
    Ok(())
}

pub fn list() -> io::Result<Vec<ManifestEntry>> {
    let manifest_path = instances_dir().join("manifest.json");
    if !manifest_path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(&manifest_path)?;
    let entries: Vec<ManifestEntry> = serde_json::from_str(&data).unwrap_or_default();
    Ok(entries)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ManifestEntry {
    pub id: String,
    pub state: String,
}

fn read_manifest() -> Vec<ManifestEntry> {
    let path = instances_dir().join("manifest.json");

    if !path.exists() {
        return Vec::new();
    }

    fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_manifest(entries: &[ManifestEntry]) -> io::Result<()> {
    let dir = instances_dir();
    fs::create_dir_all(&dir)?;

    let path = dir.join("manifest.json");
    let json = serde_json::to_string_pretty(entries).map_err(io::Error::other)?;

    fs::write(&path, json)
}

fn update_manifest(instance_id: &str, state: &str) -> io::Result<()> {
    let mut entries = read_manifest();

    entries.retain(|e| e.id != instance_id);
    entries.push(ManifestEntry {
        id: instance_id.to_string(),
        state: state.to_string(),
    });

    write_manifest(&entries)
}

fn remove_from_manifest(instance_id: &str) -> io::Result<()> {
    let mut entries = read_manifest();
    entries.retain(|e| e.id != instance_id);
    write_manifest(&entries)
}
