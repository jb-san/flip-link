mod client;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "flip", about = "flip-link CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show daemon + device status (does a PING round-trip).
    Status,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Status => {
            match client::ping_through_daemon(b"flip-status", Duration::from_secs(3)) {
                Ok(echo) => {
                    println!("daemon: up");
                    println!("device: reachable (PONG round-trip ok)");
                    println!("echo:   {:?}", String::from_utf8_lossy(&echo));
                }
                Err(e) => {
                    // Don't claim the daemon is up — the failure may be that it
                    // never came up. Report the underlying reason honestly.
                    println!("status: FAILED — {e:#}");
                    std::process::exit(1);
                }
            }
            Ok(())
        }
    }
}
