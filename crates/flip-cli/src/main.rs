mod kv;

use anyhow::Result;
use clap::{Parser, Subcommand};
use flip_client::{
    subghz::parse_probe_hex, DaemonStatus, DeviceStatus, IrSignal, SubGhzPreset, SubGhzSignal,
};
use flip_proto::Value;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Parser)]
#[command(name = "flip", about = "flip-link CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
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
    /// Sub-GHz radio commands.
    #[command(name = "subghz", alias = "sub-ghz")]
    SubGhz {
        #[command(subcommand)]
        cmd: SubGhzCmd,
    },
}

#[derive(Debug, Subcommand)]
enum DaemonCmd {
    /// Report whether the daemon is running and the device connected.
    Status,
}

#[derive(Debug, Subcommand)]
enum IrCmd {
    /// Transmit an IR signal record from a file.
    Transmit {
        /// Path to an IR signal record file.
        #[arg(long)]
        file: String,
        /// Override carrier frequency in Hz from the signal file.
        #[arg(long)]
        freq: Option<u32>,
        /// Override duty cycle in permille from the signal file.
        #[arg(long)]
        duty: Option<u32>,
    },
    /// Capture an IR signal record to a file (or stdout). Stops on Ctrl-C, an
    /// optional fixed duration, or an optional post-data silence gap.
    Capture {
        /// Write signal record here (default: stdout).
        #[arg(long)]
        output: Option<String>,
        /// Stop after this many ms of silence once IR data has been seen.
        #[arg(long)]
        idle_gap: Option<u64>,
        /// Stop after this many ms of wall-clock capture time.
        #[arg(long)]
        duration: Option<u64>,
    },
}

#[derive(Debug, Subcommand)]
enum SubGhzCmd {
    /// Capture a raw Sub-GHz level/duration record.
    Capture {
        /// Frequency in Hz. Required; there is no default RF frequency.
        #[arg(long)]
        freq: u32,
        /// Radio preset: ook270, ook650, 2fsk_dev238, 2fsk_dev476, msk99_97, gfsk9_99.
        #[arg(long)]
        preset: String,
        /// Write signal record here (default: stdout).
        #[arg(long)]
        output: Option<String>,
        /// Stop after this many ms of silence once Sub-GHz data has been seen.
        #[arg(long)]
        idle_gap: Option<u64>,
        /// Stop after this many ms of wall-clock capture time.
        #[arg(long)]
        duration: Option<u64>,
    },
    /// Transmit a raw Sub-GHz level/duration record from a file.
    Transmit {
        /// Path to a Sub-GHz signal record file.
        #[arg(long)]
        file: String,
        /// Override frequency in Hz from the signal file.
        #[arg(long)]
        freq: Option<u32>,
        /// Override preset from the signal file.
        #[arg(long)]
        preset: Option<String>,
        /// Repeat count.
        #[arg(long, default_value_t = 1)]
        repeat: u32,
    },
    /// Probe the SDK Sub-GHz byte worker with a small payload.
    LinkProbe {
        /// Frequency in Hz. Required; there is no default RF frequency.
        #[arg(long)]
        freq: u32,
        /// UTF-8 text payload to write.
        #[arg(long, conflicts_with = "hex", required_unless_present = "hex")]
        data: Option<String>,
        /// Hex payload to write, for example 0x68656c6c6f.
        #[arg(long, conflicts_with = "data")]
        hex: Option<String>,
        /// Firmware wait after writing, in ms.
        #[arg(long, default_value_t = 500)]
        timeout: u64,
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
            IrCmd::Capture {
                output,
                idle_gap,
                duration,
            } => run_capture(idle_gap, duration, output.as_deref()),
        },
        Cmd::SubGhz { cmd } => match cmd {
            SubGhzCmd::Capture {
                freq,
                preset,
                output,
                idle_gap,
                duration,
            } => run_subghz_capture(freq, &preset, idle_gap, duration, output.as_deref()),
            SubGhzCmd::Transmit {
                file,
                freq,
                preset,
                repeat,
            } => {
                let mut signal = SubGhzSignal::read_file(&file)?;
                if let Some(freq) = freq {
                    signal.frequency = freq;
                }
                if let Some(preset) = preset {
                    signal.preset = preset.parse::<SubGhzPreset>()?;
                }
                let count = signal.edges.len();
                let sent = flip_client::subghz_transmit(&signal, repeat, Duration::from_secs(30))?;
                println!("transmitted {count} Sub-GHz edges: {sent}");
                Ok(())
            }
            SubGhzCmd::LinkProbe {
                freq,
                data,
                hex,
                timeout,
            } => {
                let payload = match (data, hex) {
                    (Some(data), None) => data.into_bytes(),
                    (None, Some(hex)) => parse_probe_hex(&hex)?,
                    _ => unreachable!("clap enforces exactly one payload source"),
                };
                let result =
                    flip_client::subghz_link_probe(freq, &payload, Duration::from_millis(timeout))?;
                println!(
                    "link probe wrote {} bytes; read {} bytes; callbacks {}",
                    result.written, result.read, result.callbacks
                );
                if !result.rx_preview.is_empty() {
                    println!("rx preview: {}", hex_preview(&result.rx_preview));
                }
                Ok(())
            }
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

fn run_capture(
    idle_gap_ms: Option<u64>,
    duration_ms: Option<u64>,
    output: Option<&str>,
) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        let _ = ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst));
    }

    eprintln!("capturing... (Ctrl-C to stop)");
    let idle_gap = idle_gap_ms.map(Duration::from_millis);
    let duration = duration_ms.map(Duration::from_millis);
    let started = Instant::now();
    let signal = flip_client::ir_capture(idle_gap, &|| {
        stop.load(Ordering::SeqCst)
            || duration
                .map(|duration| started.elapsed() >= duration)
                .unwrap_or(false)
    })?;

    match output {
        Some(path) => {
            signal.write_file(path)?;
            eprintln!("captured {} timings -> {path}", signal.timings.len());
        }
        None => std::io::stdout().write_all(signal.to_file_string().as_bytes())?,
    }
    Ok(())
}

fn run_subghz_capture(
    freq: u32,
    preset: &str,
    idle_gap_ms: Option<u64>,
    duration_ms: Option<u64>,
    output: Option<&str>,
) -> Result<()> {
    let preset = preset.parse::<SubGhzPreset>()?;
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        let _ = ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst));
    }

    eprintln!("capturing Sub-GHz... (Ctrl-C to stop)");
    let idle_gap = idle_gap_ms.map(Duration::from_millis);
    let duration = duration_ms.map(Duration::from_millis);
    let started = Instant::now();
    let signal = flip_client::subghz_capture(freq, preset, idle_gap, &|| {
        stop.load(Ordering::SeqCst)
            || duration
                .map(|duration| started.elapsed() >= duration)
                .unwrap_or(false)
    })?;

    match output {
        Some(path) => {
            signal.write_file(path)?;
            eprintln!("captured {} Sub-GHz edges -> {path}", signal.edges.len());
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

fn hex_preview(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ir_capture_accepts_idle_gap_and_duration() {
        let cli = Cli::try_parse_from([
            "flip",
            "ir",
            "capture",
            "--idle-gap",
            "800",
            "--duration",
            "5000",
            "--output",
            "/tmp/raw.ir",
        ])
        .unwrap();

        let Cmd::Ir {
            cmd:
                IrCmd::Capture {
                    output,
                    idle_gap,
                    duration,
                },
        } = cli.cmd
        else {
            panic!("expected ir capture command");
        };

        assert_eq!(output.as_deref(), Some("/tmp/raw.ir"));
        assert_eq!(idle_gap, Some(800));
        assert_eq!(duration, Some(5000));
    }

    #[test]
    fn ir_capture_rejects_removed_auto_end_flag() {
        let err = match Cli::try_parse_from(["flip", "ir", "capture", "--auto-end", "800"]) {
            Ok(_) => panic!("--auto-end should no longer parse"),
            Err(err) => err,
        };

        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn subghz_capture_requires_freq_and_accepts_preset() {
        let cli = Cli::try_parse_from([
            "flip",
            "subghz",
            "capture",
            "--freq",
            "433920000",
            "--preset",
            "ook650",
            "--idle-gap",
            "500",
            "--output",
            "/tmp/button.subghz",
        ])
        .unwrap();

        let Cmd::SubGhz {
            cmd:
                SubGhzCmd::Capture {
                    freq,
                    preset,
                    idle_gap,
                    output,
                    ..
                },
        } = cli.cmd
        else {
            panic!("expected subghz capture");
        };

        assert_eq!(freq, 433_920_000);
        assert_eq!(preset, "ook650");
        assert_eq!(idle_gap, Some(500));
        assert_eq!(output.as_deref(), Some("/tmp/button.subghz"));
    }

    #[test]
    fn subghz_transmit_accepts_repeat() {
        let cli = Cli::try_parse_from([
            "flip",
            "subghz",
            "transmit",
            "--file",
            "/tmp/button.subghz",
            "--repeat",
            "2",
        ])
        .unwrap();

        let Cmd::SubGhz {
            cmd: SubGhzCmd::Transmit { file, repeat, .. },
        } = cli.cmd
        else {
            panic!("expected subghz transmit");
        };

        assert_eq!(file, "/tmp/button.subghz");
        assert_eq!(repeat, 2);
    }

    #[test]
    fn subghz_link_probe_accepts_data_payload() {
        let cli = Cli::try_parse_from([
            "flip",
            "subghz",
            "link-probe",
            "--freq",
            "433920000",
            "--data",
            "hello",
            "--timeout",
            "250",
        ])
        .unwrap();

        let Cmd::SubGhz {
            cmd:
                SubGhzCmd::LinkProbe {
                    freq,
                    data,
                    hex,
                    timeout,
                },
        } = cli.cmd
        else {
            panic!("expected subghz link-probe");
        };

        assert_eq!(freq, 433_920_000);
        assert_eq!(data.as_deref(), Some("hello"));
        assert_eq!(hex, None);
        assert_eq!(timeout, 250);
    }

    #[test]
    fn subghz_link_probe_requires_one_payload_source() {
        let missing = Cli::try_parse_from(["flip", "subghz", "link-probe", "--freq", "433920000"])
            .unwrap_err();
        assert_eq!(
            missing.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );

        let conflict = Cli::try_parse_from([
            "flip",
            "subghz",
            "link-probe",
            "--freq",
            "433920000",
            "--data",
            "hello",
            "--hex",
            "6869",
        ])
        .unwrap_err();
        assert_eq!(conflict.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
}
