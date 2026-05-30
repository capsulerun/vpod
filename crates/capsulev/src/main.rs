mod pull;
mod registry;
mod run;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use registry::DEFAULT_REGISTRY;

#[derive(Parser)]
#[command(name = "capsulev", about = "RISC-V Linux sandbox")]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,

    #[arg(long, env = "CAPSULEV_REGISTRY", global = true, hide = true)]
    registry: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    Start {
        #[arg(default_value = "alpine")]
        snapshot: String,

        #[arg(long)]
        disk: Option<PathBuf>,

        #[arg(long, env = "CAPSULEV_SNAPSHOT")]
        local: Option<PathBuf>,
    },

    Pull {
        #[arg(default_value = "alpine")]
        snapshot: String,
    },

    List,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let reg_url = cli.registry.as_deref().unwrap_or(DEFAULT_REGISTRY);

    match cli.command.unwrap_or(Cmd::Start {
        snapshot: "alpine".to_string(),
        disk: None,
        local: None,
    }) {
        Cmd::Start {
            snapshot: snapshot_name,
            disk,
            local,
        } => {
            let snapshot = if let Some(path) = local {
                if !path.exists() {
                    anyhow::bail!("local snapshot not found: {}", path.display());
                }

                registry::Snapshot {
                    name: snapshot_name.split(':').next().unwrap_or(&snapshot_name).to_string(),
                    tag: snapshot_name.split(':').nth(1).unwrap_or("local").to_string(),
                    memory_label: "256MB".to_string(),
                    description: path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("local snapshot")
                        .to_string(),
                    url: path.to_str().unwrap().to_string(),
                    sha256: String::new(),
                    size: 0,
                }
            } else {
                resolve_snapshot(&snapshot_name, reg_url)?
            };

            run::run(run::RunConfig {
                snapshot,
                disk,
                command: None,
            })?;
        }

        Cmd::Pull { snapshot } => {
            let snapshots = registry::fetch(reg_url)
                .context("failed to fetch registry — cannot pull without registry")?;
            let snap = registry::resolve(&snapshots, &snapshot)
                .with_context(|| format!("unknown snapshot '{snapshot}' — run `capsulev list`"))?;

            if pull::is_cached(snap) {
                eprintln!("'{}' is already cached at {}", snap.display_name(), pull::snapshot_path(snap).display());
            } else {
                pull::pull(snap)?;
            }
        }

        Cmd::List => {
            match registry::fetch(reg_url) {
                Ok(snapshots) => {
                    println!(
                        "{:<20} {:<12} {:<10} {} {}",
                        "NAME", "TAG", "MEMORY", "DESCRIPTION", "STATUS"
                    );

                    for snap in &snapshots {
                        let (status, desc_style) = if pull::is_cached(snap) {
                            ("✓ cached", "")
                        } else {
                            ("  remote", "\x1b[2m")
                        };
                        println!(
                            "{:<20} {:<12} {:<10} {}{}\x1b[0m {}",
                            snap.display_name(),
                            snap.tag,
                            snap.memory_label,
                            desc_style,
                            snap.description,
                            status,
                        );
                    }
                }
                Err(_) => {
                    eprintln!("\x1b[2mRegistry unavailable, showing local snapshots:\x1b[0m");
                    println!(
                        "{:<20} {:<12} {:<10} {}",
                        "NAME", "TAG", "MEMORY", "LOCATION"
                    );

                    list_local_snapshots();
                }
            }
        }
    }

    Ok(())
}

fn resolve_snapshot(name: &str, reg_url: &str) -> Result<registry::Snapshot> {
    if let Ok(snapshots) = registry::fetch(reg_url) {
        if let Some(snap) = registry::resolve(&snapshots, name) {

            let path = if pull::is_cached(snap) {
                pull::snapshot_path(snap)
            } else {
                eprintln!("Snapshot '{}' not found locally, downloading...", snap.display_name());
                pull::pull(snap)?
            };

            let mut snap = snap.clone();
            snap.url = path.to_str().unwrap().to_string();
            return Ok(snap);
        }
    }

    resolve_snapshot_local(name)
}

fn resolve_snapshot_local(name: &str) -> Result<registry::Snapshot> {
    if let Ok(path_str) = std::env::var("CAPSULEV_SNAPSHOT") {
        let path = PathBuf::from(&path_str);
        if path.exists() {
            return Ok(registry::Snapshot {
                name: name.split(':').next().unwrap_or(name).to_string(),
                tag: name.split(':').nth(1).unwrap_or("local").to_string(),
                memory_label: "256MB".to_string(),
                description: path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("local snapshot")
                    .to_string(),
                url: path_str,
                sha256: String::new(),
                size: 0,
            });
        }
    }

    // Check dev fallback paths: dist/<name>.snap
    let snap_name = name.split(':').next().unwrap_or(name);
    for base in [
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf())),
        Some(PathBuf::from(".")),
        Some(PathBuf::from("dist")),
    ]
    .into_iter()
    .flatten()
    {
        let candidate = base.join(format!("{snap_name}.snap"));
        if candidate.exists() {
            return Ok(registry::Snapshot {
                name: snap_name.to_string(),
                tag: name.split(':').nth(1).unwrap_or("dev").to_string(),
                memory_label: "256MB".to_string(),
                description: "local dev snapshot".to_string(),
                url: candidate.to_str().unwrap().to_string(),
                sha256: String::new(),
                size: 0,
            });
        }
    }

    anyhow::bail!("snapshot '{}' not found locally", name)
}

fn list_local_snapshots() {
    if let Ok(path_str) = std::env::var("CAPSULEV_SNAPSHOT") {
        let path = PathBuf::from(&path_str);
        if path.exists() {
            let name = path.file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");
            println!(
                "{:<20} {:<12} {:<10} {}",
                name, "local", "256MB", path.display()
            );
        }
    }

    for base in [
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf())),
        Some(PathBuf::from(".")),
        Some(PathBuf::from("dist")),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("snap") {
                    let name = path.file_stem()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown");
                    println!(
                        "{:<20} {:<12} {:<10} {}",
                        name, "dev", "256MB", path.display()
                    );
                }
            }
        }
    }
}
