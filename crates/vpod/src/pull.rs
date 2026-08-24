use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};

use crate::registry::{self, AuthRefused, Snapshot};

pub fn cache_dir() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("~/.local/share"));
    base.join("vpod").join("snapshots")
}

pub fn snapshot_path(snap: &Snapshot) -> PathBuf {
    cache_dir().join(format!("{}.snap", snap.id))
}

pub fn meta_path(snap: &Snapshot) -> PathBuf {
    cache_dir().join(format!("{}.meta", snap.id))
}

pub fn origin_path(snap: &Snapshot) -> PathBuf {
    cache_dir().join(format!("{}.origin", snap.id))
}

fn record_origin(snap: &Snapshot, origin: &str) {
    let path = origin_path(snap);
    if path.exists() {
        return;
    }
    if let Err(unwritable) = fs::write(&path, origin) {
        eprintln!(
            "vpod: cannot record the origin of {}: {unwritable}",
            path.display()
        );
    }
}

pub fn is_cached(snap: &Snapshot) -> bool {
    let dest = snapshot_path(snap);
    let meta = meta_path(snap);
    if dest.exists() && meta.exists() {
        fs::read_to_string(&meta)
            .map(|s| s.trim() == snap.sha256)
            .unwrap_or(false)
    } else {
        false
    }
}

pub fn pull(snap: &Snapshot, registry_url: &str, api_key: Option<&str>) -> Result<PathBuf> {
    let dest = snapshot_path(snap);
    fs::create_dir_all(dest.parent().unwrap())?;

    let tmp = dest.with_extension("snap.tmp");

    let client = reqwest::blocking::Client::new();
    let resp = registry::authorize(client.get(&snap.url), &snap.url, registry_url, api_key)
        .send()
        .with_context(|| format!("failed to download {}", snap.url))?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(anyhow::Error::new(AuthRefused {
            status: status.as_u16(),
            url: snap.url.clone(),
        }));
    }

    if !status.is_success() {
        anyhow::bail!("download failed: {status}");
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

    let mut file =
        fs::File::create(&tmp).with_context(|| format!("failed to create {}", tmp.display()))?;

    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0;
    let bytes = resp.bytes().context("failed to read response body")?;

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

    let meta = meta_path(snap);
    fs::write(&meta, &snap.sha256)?;

    eprintln!("Pulled {} → {}", snap.display_name(), dest.display());
    Ok(dest)
}

pub fn record_pull_origin(snap: &Snapshot, origin: &str) {
    record_origin(snap, origin);
}

pub fn prune_stale(registry_snapshots: &[Snapshot], current_origin: &str) {
    let known_ids: std::collections::HashSet<&str> =
        registry_snapshots.iter().map(|s| s.id.as_str()).collect();
    let referenced = snapshots_referenced_by_instances();

    let Ok(entries) = fs::read_dir(cache_dir()) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.ends_with(".tmp") {
            fs::remove_file(&path).ok();
            continue;
        }

        let is_snapshot_artifact = path
            .extension()
            .is_some_and(|ext| ext == "snap" || ext == "raw");
        if !is_snapshot_artifact {
            continue;
        }

        let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if known_ids.contains(id) || referenced.contains(id) {
            continue;
        }

        let meta = path.with_extension("meta");
        if !meta.exists() {
            continue;
        }

        let origin = path.with_extension("origin");
        let Ok(recorded_origin) = fs::read_to_string(&origin) else {
            continue;
        };
        if recorded_origin.trim() != current_origin {
            continue;
        }

        eprintln!("vpod: removing {name}, which we downloaded and the registry no longer lists");
        fs::remove_file(&path).ok();
        fs::remove_file(&meta).ok();
        fs::remove_file(&origin).ok();
    }
}

fn snapshots_referenced_by_instances() -> std::collections::HashSet<String> {
    let mut referenced = std::collections::HashSet::new();
    let Some(home) = dirs::home_dir() else {
        return referenced;
    };

    let instances_dir = home.join(".vpod").join("instances");
    let Ok(entries) = fs::read_dir(&instances_dir) else {
        return referenced;
    };

    for entry in entries.flatten() {
        let meta_path = entry.path().join("meta.json");
        let Ok(meta_text) = fs::read_to_string(&meta_path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta_text) else {
            continue;
        };

        if let Some(snapshot_ref) = meta.get("snapshot").and_then(|v| v.as_str()) {
            let id = snapshot_ref
                .trim_start_matches("snap/")
                .trim_end_matches(".snap");
            referenced.insert(id.to_string());
        }
    }

    referenced
}
