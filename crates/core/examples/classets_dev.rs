//! Dev CLI for the CLASSETS sync engine.
//!
//! ```text
//! cargo run -p classets-core --example classets_dev -- pair  --server URL --root DIR --data DIR --name NAME
//! cargo run -p classets-core --example classets_dev -- sync  --data DIR [--root DIR]
//! cargo run -p classets-core --example classets_dev -- watch --data DIR [--root DIR]
//! cargo run -p classets-core --example classets_dev -- status --data DIR
//! ```
//! `pair` saves `config.json` in the data dir, so later commands only need `--data`.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use classets_core::{Engine, EngineConfig, PairStatus};

#[derive(Parser)]
#[command(name = "classets_dev", about = "CLASSETS sync engine dev tool")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Pair this device: prints the code and URL, polls until approved.
    Pair {
        #[arg(long)]
        server: String,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        name: String,
    },
    /// One sync pass; prints the report as JSON.
    Sync {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Sync, then keep watching until Ctrl-C.
    Watch {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Print paired state and the last report.
    Status {
        #[arg(long)]
        data: PathBuf,
    },
}

fn load(data: PathBuf, root: Option<PathBuf>) -> Result<Engine, Box<dyn std::error::Error>> {
    let mut config = EngineConfig::load(&data)
        .map_err(|e| format!("no config in {}: {e} (run `pair` first)", data.display()))?;
    config.data_dir = data;
    if let Some(root) = root {
        config.sync_root = root;
    }
    Ok(Engine::new(config)?)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().cmd {
        Cmd::Pair {
            server,
            root,
            data,
            name,
        } => {
            let config = EngineConfig {
                server_url: server,
                sync_root: root,
                data_dir: data,
                device_name: name,
            };
            config.save()?;
            let engine = Engine::new(config)?;
            let start = engine.pair_start().await?;
            println!(
                "Code: {}\nApprove at: {}\n(expires in {} s)",
                start.user_code, start.verification_url, start.expires_in
            );
            loop {
                tokio::time::sleep(Duration::from_secs(start.interval.max(1))).await;
                match engine.pair_poll().await? {
                    PairStatus::Pending => eprint!("."),
                    PairStatus::Approved { device_id } => {
                        println!("\nPaired as device {device_id}");
                        return Ok(());
                    }
                    PairStatus::Expired { reason } => {
                        return Err(format!("pairing {reason}").into());
                    }
                }
            }
        }
        Cmd::Sync { data, root } => {
            let report = load(data, root)?.sync_once().await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Cmd::Watch { data, root } => {
            let engine = load(data, root)?;
            eprintln!(
                "Watching {} (Ctrl-C to stop)",
                engine.config().sync_root.display()
            );
            engine
                .run_loop(
                    async {
                        let _ = tokio::signal::ctrl_c().await;
                    },
                    |r| eprintln!("{}", serde_json::to_string(r).unwrap_or_default()),
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&engine.status())?);
        }
        Cmd::Status { data } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&load(data, None)?.status())?
            );
        }
    }
    Ok(())
}
