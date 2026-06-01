use anyhow::{Context, Result};
use serde::Deserialize;

pub const DEFAULT_REGISTRY: &str =
    "https://capsulerun.github.io/wasm-linux-snapshots/registry.json";

#[derive(Debug, Deserialize, Clone)]
pub struct Snapshot {
    pub id: String,
    pub name: String,
    pub tag: String,
    pub memory_label: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,

    #[serde(default)]
    pub description: String,
}

impl Snapshot {
    pub fn display_name(&self) -> String {
        self.name.to_string()
    }
}

#[derive(Debug, Deserialize)]
struct Registry {
    version: String,
    snapshots: Vec<Snapshot>,
}

pub fn fetch(registry_url: &str) -> Result<(String, Vec<Snapshot>)> {
    let resp = reqwest::blocking::get(registry_url)
        .with_context(|| format!("failed to fetch registry from {registry_url}"))?;

    if !resp.status().is_success() {
        anyhow::bail!("registry request failed: {}", resp.status());
    }

    let reg: Registry = resp.json().context("failed to parse registry JSON")?;
    Ok((reg.version, reg.snapshots))
}

pub fn resolve<'a>(snapshots: &'a [Snapshot], name: &str) -> Option<&'a Snapshot> {
    let (want_name, want_tag) = match name.split_once(':') {
        Some((n, t)) => (n, Some(t)),
        None => (name, None),
    };

    snapshots
        .iter()
        .find(|s| s.id == name || (s.name == want_name && want_tag.is_none_or(|t| t == "latest" || t == s.tag)))
}
