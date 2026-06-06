mod client;
mod ir;
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
    /// Daemon control.
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
    /// IR instrument commands.
    Ir {
        #[command(subcommand)]
        cmd: IrCmd,
    },
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Report whether the daemon is running and the device connected.
    Status,
}

#[derive(Subcommand)]
enum IrCmd {
    /// Transmit IR timings from a file.
    Transmit {
        /// Path to a file of whitespace/newline-separated µs timings.
        #[arg(long)]
        file: String,
        /// Carrier frequency in Hz.
        #[arg(long, default_value_t = 38000)]
        freq: u64,
        /// Duty cycle in permille (e.g. 330 = 33%).
        #[arg(long, default_value_t = 330)]
        duty: u64,
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
        Cmd::Daemon { cmd } => match cmd {
            DaemonCmd::Status => {
                client::daemon_status();
                Ok(())
            }
        },
        Cmd::Ir { cmd } => match cmd {
            IrCmd::Transmit { file, freq, duty } => {
                let timings = ir::load_timings_file(&file)?;
                let count = timings.len();
                let params = ir::transmit_params(timings, freq, duty);
                let resp = client::invoke("ir", "transmit", params, Duration::from_secs(10))?;
                println!("transmitted {count} edges: {}", client::render_value(&resp.result));
                Ok(())
            }
        },
    }
}
