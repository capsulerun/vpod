mod pull;
mod registry;
mod run;

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
        #[arg(default_value = "alpine-256mb")]
        snapshot: String,
    },

    Pull {
        #[arg(default_value = "alpine-256mb")]
        snapshot: String,
    },

    List,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let reg_url = cli.registry.as_deref().unwrap_or(DEFAULT_REGISTRY);

    match cli.command.unwrap_or(Cmd::Start {
        snapshot: "alpine-3.23.0-256mb".to_string(),
    }) {
        Cmd::Start {
            snapshot: snapshot_name,
        } => {
            let (version, snapshot) = resolve_snapshot(&snapshot_name, reg_url)?;

            run::run(run::RunConfig { version, snapshot })?;
        }

        Cmd::Pull { snapshot } => {
            let (_, snapshots) = registry::fetch(reg_url)
                .context("failed to fetch registry — cannot pull without registry")?;
            let snap = registry::resolve(&snapshots, &snapshot)
                .with_context(|| format!("unknown snapshot '{snapshot}' — run `capsulev list`"))?;

            if pull::is_cached(snap) {
                eprintln!(
                    "'{}' is already cached at {}",
                    snap.display_name(),
                    pull::snapshot_path(snap).display()
                );
            } else {
                pull::pull(snap)?;
            }
        }

        Cmd::List => match registry::fetch(reg_url) {
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
    }

    Ok(())
}

fn resolve_snapshot(name: &str, reg_url: &str) -> Result<(String, registry::Snapshot)> {
    if let Ok((version, snapshots)) = registry::fetch(reg_url)
        && let Some(snap) = registry::resolve(&snapshots, name)
    {
        let path = if pull::is_cached(snap) {
            pull::snapshot_path(snap)
        } else {
            pull::pull(snap)?
        };

        let mut snap = snap.clone();
        snap.url = path.to_str().unwrap().to_string();
        return Ok((version, snap));
    }

    anyhow::bail!("snapshot '{}' not found in registry", name)
}

// fn list_local_snapshots() {
//     for base in [
//         std::env::current_exe()
//             .ok()
//             .and_then(|p| p.parent().map(|p| p.to_path_buf())),
//         Some(PathBuf::from(".")),
//         Some(PathBuf::from("dist")),
//     ]
//     .into_iter()
//     .flatten()
//     {
//         if let Ok(entries) = std::fs::read_dir(&base) {
//             for entry in entries.flatten() {
//                 let path = entry.path();
//                 if path.extension().and_then(|e| e.to_str()) == Some("snap") {
//                     let name = path.file_stem()
//                         .and_then(|n| n.to_str())
//                         .unwrap_or("unknown");
//                     println!(
//                         "{:<20} {:<12} {:<10} {}",
//                         name, "dev", "256MB", path.display()
//                     );
//                 }
//             }
//         }
//     }
// }
