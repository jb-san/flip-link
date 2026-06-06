mod client;
mod kv;

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
    /// List instruments and opcodes the device advertises.
    Caps,
    /// Invoke an instrument opcode with optional key=value params.
    Invoke {
        instrument: String,
        opcode: String,
        /// Zero or more key=value params.
        params: Vec<String>,
    },
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
        Cmd::Caps => {
            let caps = client::caps(Duration::from_secs(3))?;
            println!("protocol v{}", caps.protocol_version);
            for inst in &caps.instruments {
                println!("{}", inst.id);
                for op in &inst.opcodes {
                    println!("  {}.{op}", inst.id);
                }
            }
            Ok(())
        }
        Cmd::Invoke {
            instrument,
            opcode,
            params,
        } => {
            let params = kv::parse_params(&params)?;
            let resp = client::invoke(&instrument, &opcode, params, Duration::from_secs(3))?;
            println!("{}", client::render_value(&resp.result));
            Ok(())
        }
    }
}
