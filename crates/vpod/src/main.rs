mod pull;
mod registry;
mod start;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "vpod",
    about = "Lightweight, secure sandboxes for untrusted processes."
)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,

    #[arg(long, env = "VPOD_REGISTRY", global = true, hide = true)]
    registry: Option<String>,

    #[arg(long = "api-key", env = "VPOD_API_KEY", global = true)]
    api_key: Option<String>,

    #[arg(short = 'm', long = "mount", global = true)]
    mounts: Vec<String>,
}

#[derive(Subcommand)]
enum Cmd {
    Start {
        #[arg(default_value = "vpod-base")]
        snapshot: String,
    },

    Pull {
        #[arg(default_value = "vpod-base")]
        snapshot: String,
    },

    List,

    #[command(external_subcommand)]
    External(Vec<String>),
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let api_key = cli.api_key.as_deref().filter(|key| !key.is_empty());
    if let Some(key) = api_key {
        registry::check_api_key_kind(key)?;
    }

    let reg_url = registry::resolve_registry_url(cli.registry.as_deref(), api_key);
    let reg_url = reg_url.as_str();
    let origin = registry::origin_tag(reg_url, api_key);

    let mounts = cli.mounts;

    match cli.command.unwrap_or(Cmd::Start {
        snapshot: "alpine-3.23.0-256mb".to_string(),
    }) {
        Cmd::Start {
            snapshot: snapshot_name,
        } => {
            let (version, snapshot) = resolve_snapshot(&snapshot_name, reg_url, api_key, &origin)?;
            let parsed_mounts = start::parse_mounts(&mounts)?;

            start::run(start::RunConfig {
                version,
                snapshot,
                mounts: parsed_mounts,
            })?;
        }

        Cmd::Pull { snapshot } => {
            let (_, snapshots) = registry::fetch(reg_url, api_key)
                .context("failed to fetch registry — cannot pull without registry")?;
            let snap = registry::resolve(&snapshots, &snapshot)
                .with_context(|| {
                    registry::not_found_message(&snapshot, reg_url, api_key.is_some())
                })?
                .clone();

            if pull::is_cached(&snap) {
                pull::record_pull_origin(&snap, &origin);
                eprintln!(
                    "'{}' is already cached at {}",
                    snap.display_name(),
                    pull::snapshot_path(&snap).display()
                );
            } else {
                let fresh = pull_with_one_retry(&snap, &snapshot, reg_url, api_key, snapshots)?;
                pull::record_pull_origin(&snap, &origin);
                pull::prune_stale(&fresh, &origin);
            }
        }

        Cmd::List => match registry::fetch(reg_url, api_key) {
            Ok((_, snapshots)) => {
                println!(
                    "{:<25} {:<15} {:<12} {:<10} DESCRIPTION STATUS",
                    "ID", "NAME", "TAG", "MEMORY"
                );

                for snap in &snapshots {
                    let (status, desc_style) = if pull::is_cached(snap) {
                        ("✓ cached", "")
                    } else {
                        ("  remote", "\x1b[2m")
                    };
                    println!(
                        "{:<25} {:<15} {:<12} {:<10} {}{}\x1b[0m {}",
                        snap.id,
                        snap.name,
                        snap.tag,
                        snap.memory_label,
                        desc_style,
                        snap.description,
                        status,
                    );
                }
            }
            Err(_) => {
                eprintln!("\x1b[2mRegistry unreachable\x1b[0m");
            }
        },

        Cmd::External(args) => {
            let snapshot_name = args.first().map(|s| s.as_str()).unwrap_or("vpod-base");
            let (version, snapshot) = resolve_snapshot(snapshot_name, reg_url, api_key, &origin)?;
            let parsed_mounts = start::parse_mounts(&mounts)?;

            start::run(start::RunConfig {
                version,
                snapshot,
                mounts: parsed_mounts,
            })?;
        }
    }

    Ok(())
}

fn pull_with_one_retry(
    snap: &registry::Snapshot,
    requested: &str,
    reg_url: &str,
    api_key: Option<&str>,
    snapshots: Vec<registry::Snapshot>,
) -> Result<Vec<registry::Snapshot>> {
    match pull::pull(snap, reg_url, api_key) {
        Ok(_) => Ok(snapshots),
        Err(refused) if registry::is_auth_refused(&refused) => {
            let (_, fresh) = registry::fetch(reg_url, api_key)
                .context("the snapshot was refused, and the catalogue could not be refreshed")?;
            let snap = registry::resolve(&fresh, requested).with_context(|| {
                registry::not_found_message(requested, reg_url, api_key.is_some())
            })?;

            pull::pull(snap, reg_url, api_key).map_err(|again| {
                anyhow::anyhow!(
                    "{again}\n\nRefused again after refreshing the catalogue from \
                     {reg_url}. A stale signed URL would have been fixed by that \
                     refresh, so this is the key or the organisation, not the cache."
                )
            })?;
            Ok(fresh)
        }
        Err(other) => Err(other),
    }
}

fn resolve_snapshot(
    name: &str,
    reg_url: &str,
    api_key: Option<&str>,
    origin: &str,
) -> Result<(String, registry::Snapshot)> {
    // Try local file first (for testing)
    // let local_paths = [
    //     std::path::PathBuf::from(&name),
    //     std::path::PathBuf::from("dist").join(format!("{}.snap", name)),
    //     std::path::PathBuf::from("dist").join(&name),
    // ];

    // for path in &local_paths {
    //     if path.exists() {
    //         return Ok(("0.1.0".to_string(), registry::Snapshot {
    //             id: "local".to_string(),
    //             name: name.to_string(),
    //             tag: "local".to_string(),
    //             memory_label: "256MB".to_string(),
    //             description: "Local snapshot".to_string(),
    //             url: path.to_str().unwrap().to_string(),
    //             size: 0,
    //             sha256: "local".to_string(),
    //         }));
    //     }
    // }

    let (version, snapshots) = registry::fetch(reg_url, api_key)?;

    let mut snap = registry::resolve(&snapshots, name)
        .with_context(|| registry::not_found_message(name, reg_url, api_key.is_some()))?
        .clone();

    let path = if pull::is_cached(&snap) {
        pull::record_pull_origin(&snap, origin);
        pull::snapshot_path(&snap)
    } else {
        let fresh = pull_with_one_retry(&snap, name, reg_url, api_key, snapshots)?;
        pull::record_pull_origin(&snap, origin);
        pull::prune_stale(&fresh, origin);
        pull::snapshot_path(&snap)
    };

    snap.url = path.to_str().unwrap().to_string();
    Ok((version, snap))
}
