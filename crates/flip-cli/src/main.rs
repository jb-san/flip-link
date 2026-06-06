mod kv;

use anyhow::Result;
use clap::{Parser, Subcommand};
use flip_client::{DaemonStatus, DeviceStatus, IrSignal};
use flip_proto::Value;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
        /// Override carrier frequency in Hz from the signal file.
        #[arg(long)]
        freq: Option<u32>,
        /// Override duty cycle in permille from the signal file.
        #[arg(long)]
        duty: Option<u32>,
    },
    /// Capture IR timings to a file (or stdout). Stops on Ctrl-C, or after a
    /// silence gap with --auto-end.
    Capture {
        /// Write timings here (default: stdout).
        #[arg(long)]
        output: Option<String>,
        /// Auto-stop after this many ms of silence (default: run until Ctrl-C).
        #[arg(long)]
        auto_end: Option<u64>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Status => {
            match flip_client::ping_through_daemon(b"flip-status", Duration::from_secs(3)) {
                Ok(echo) => {
                    println!("daemon: up");
                    println!("device: reachable (PONG round-trip ok)");
                    println!("echo:   {:?}", String::from_utf8_lossy(&echo));
                }
                Err(e) => {
                    println!("status: FAILED — {e:#}");
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        Cmd::Caps => {
            let caps = flip_client::caps(Duration::from_secs(3))?;
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
            let resp = flip_client::invoke(&instrument, &opcode, params, Duration::from_secs(3))?;
            println!("{}", render_value(&resp.result));
            Ok(())
        }
        Cmd::Daemon { cmd } => match cmd {
            DaemonCmd::Status => {
                print_daemon_status(flip_client::status());
                Ok(())
            }
        },
        Cmd::Ir { cmd } => match cmd {
            IrCmd::Transmit { file, freq, duty } => {
                let mut signal = IrSignal::read_file(&file)?;
                if let Some(freq) = freq {
                    signal.frequency = freq;
                }
                if let Some(duty) = duty {
                    signal.duty_permille = duty;
                }
                let count = signal.timings.len();
                let sent = flip_client::ir_transmit(&signal, Duration::from_secs(10))?;
                println!("transmitted {count} edges: {sent}");
                Ok(())
            }
            IrCmd::Capture { output, auto_end } => run_capture(auto_end, output.as_deref()),
        },
    }
}

fn print_daemon_status(status: DaemonStatus) {
    if !status.daemon_running {
        println!("daemon: not running");
        println!("log:    {}", status.log_path.display());
        return;
    }

    println!("daemon: running");
    match status.device {
        DeviceStatus::Connected { instruments } => {
            println!("device: connected ({instruments} instruments)");
        }
        DeviceStatus::Disconnected => println!("device: disconnected"),
        DeviceStatus::Unknown(reason) => println!("device: unknown ({reason})"),
    }
    println!("log:    {}", status.log_path.display());
}

fn run_capture(auto_end_ms: Option<u64>, output: Option<&str>) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        let _ = ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst));
    }

    eprintln!("capturing... (Ctrl-C to stop)");
    let auto_end = auto_end_ms.map(Duration::from_millis);
    let signal = flip_client::ir_capture(auto_end, &|| stop.load(Ordering::SeqCst))?;

    match output {
        Some(path) => {
            signal.write_file(path)?;
            eprintln!("captured {} timings -> {path}", signal.timings.len());
        }
        None => std::io::stdout().write_all(signal.to_file_string().as_bytes())?,
    }
    Ok(())
}

fn render_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::U64(n) => n.to_string(),
        Value::I64(n) => n.to_string(),
        Value::Text(s) => s.clone(),
        Value::Bytes(bytes) => format!(
            "0x{}",
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ),
        Value::Array(items) => {
            let inner = items
                .iter()
                .map(render_value)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{inner}]")
        }
        Value::Map(fields) => {
            let inner = fields
                .iter()
                .map(|(key, value)| format!("{key}: {}", render_value(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{inner}}}")
        }
    }
}
