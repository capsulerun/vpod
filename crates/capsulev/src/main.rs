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
            snapshot,
            disk,
            local,
        } => {
            let snap_path = if let Some(path) = local {
                if !path.exists() {
                    anyhow::bail!("local snapshot not found: {}", path.display());
                }

                path
            } else {
                resolve_snapshot(&snapshot, reg_url)?
            };

            run::run(run::RunConfig {
                snapshot_name: snapshot.clone(),
                snapshot: snap_path,
                disk,
                agent: false,
                command: None,
            })?;
        }

        Cmd::Pull { snapshot } => {
            let snapshots = registry::fetch(reg_url)?;
            let snap = registry::resolve(&snapshots, &snapshot)
                .with_context(|| format!("unknown snapshot '{snapshot}' — run `capsulev list`"))?;

            if pull::is_cached(snap) {
                eprintln!("'{}' is already cached at {}", snap.display_name(), pull::snapshot_path(snap).display());
            } else {
                pull::pull(snap)?;
            }
        }

        Cmd::List => {
            let snapshots = registry::fetch(reg_url)?;
            println!(
                "{:<20} {:<10} {} {}",
                "NAME", "SIZE", "DESCRIPTION", "STATUS"
            );

            for snap in &snapshots {
                let (status, desc_style) = if pull::is_cached(snap) {
                    ("✓ cached", "")
                } else {
                    ("  remote", "\x1b[2m")
                };
                println!(
                    "{:<20} {:<10} {}{}\x1b[0m {}",
                    snap.display_name(),
                    format_size(snap.size),
                    desc_style,
                    snap.description,
                    status,
                );
            }
        }
    }

    Ok(())
}

/// Resolve a snapshot name to a local path, checking fallbacks before hitting the registry.
fn resolve_snapshot(name: &str, reg_url: &str) -> Result<PathBuf> {
    // 1. CAPSULEV_SNAPSHOT env var (set by SDK or user)
    if let Ok(path) = std::env::var("CAPSULEV_SNAPSHOT") {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
    }

    // 2. dev fallback: dist/<name>.snap next to the binary or in cwd
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
            return Ok(candidate);
        }
    }

    // 3. registry cache
    let snapshots = registry::fetch(reg_url)
        .context("failed to fetch registry — use --local or set CAPSULEV_SNAPSHOT for offline use")?;
    let snap = registry::resolve(&snapshots, name)
        .with_context(|| format!("unknown snapshot '{name}' — run `capsulev list`"))?;

    if pull::is_cached(snap) {
        return Ok(pull::snapshot_path(snap));
    }

    // 4. download
    eprintln!("Snapshot '{}' not found locally, downloading...", snap.display_name());
    pull::pull(snap)
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.0}MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.0}KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes}B")
    }
}
