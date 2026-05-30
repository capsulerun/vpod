use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};

use crate::registry::Snapshot;

pub fn cache_dir() -> PathBuf {
    let base = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"));
    base.join("capsulev").join("snapshots")
}

pub fn snapshot_path(snap: &Snapshot) -> PathBuf {
    cache_dir().join(format!("{}-{}.snap", snap.name, snap.tag))
}

pub fn is_cached(snap: &Snapshot) -> bool {
    snapshot_path(snap).exists()
}

pub fn pull(snap: &Snapshot) -> Result<PathBuf> {
    let dest = snapshot_path(snap);
    fs::create_dir_all(dest.parent().unwrap())?;

    let tmp = dest.with_extension("snap.tmp");

    let resp = reqwest::blocking::get(&snap.url)
        .with_context(|| format!("failed to download {}", snap.url))?;

    if !resp.status().is_success() {
        anyhow::bail!("download failed: {}", resp.status());
    }

    let pb = ProgressBar::new(snap.size);
    pb.set_style(
        ProgressStyle::with_template(
            "Downloading {msg} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})",
        )
        .unwrap()
        .progress_chars("=>-"),
    );
    pb.set_message(snap.display_name());

    let mut file = fs::File::create(&tmp)
        .with_context(|| format!("failed to create {}", tmp.display()))?;

    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0;
    let bytes = resp.bytes().context("failed to read response body")?;

    // write in chunks for progress reporting
    for chunk in bytes.chunks(65536) {
        file.write_all(chunk)?;
        hasher.update(chunk);
        downloaded += chunk.len() as u64;
        pb.set_position(downloaded);
    }

    pb.finish_and_clear();

    let actual = hex::encode(hasher.finalize());
    if actual != snap.sha256 {
        fs::remove_file(&tmp).ok();
        anyhow::bail!(
            "checksum mismatch for {}: expected {} got {}",
            snap.display_name(),
            snap.sha256,
            actual
        );
    }

    fs::rename(&tmp, &dest)
        .with_context(|| format!("failed to move snapshot to {}", dest.display()))?;

    eprintln!("Pulled {} → {}", snap.display_name(), dest.display());
    Ok(dest)
}
