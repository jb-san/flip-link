use crate::transport::Transport;
use anyhow::{anyhow, Result};
use serialport::SerialPort;
use std::io::{ErrorKind, Read, Write};
use std::time::Duration;

/// List candidate Flipper CDC ports (name contains "flip", or USB VID 0x0483).
pub fn list_flipper_ports() -> Result<Vec<String>> {
    let mut names: Vec<String> = serialport::available_ports()?
        .into_iter()
        .filter(|p| {
            let is_flip_name = p.port_name.to_lowercase().contains("flip");
            let is_flip_vid = matches!(
                &p.port_type,
                serialport::SerialPortType::UsbPort(info) if info.vid == 0x0483
            );
            is_flip_name || is_flip_vid
        })
        .map(|p| p.port_name)
        .collect();
    names.sort();
    Ok(names)
}

/// Pick the agent (interface-1) port. After the dual-CDC switch two flipper
/// ports exist; interface 1 is the higher-numbered one. Override with FLIP_PORT.
pub fn pick_agent_port() -> Result<String> {
    if let Ok(p) = std::env::var("FLIP_PORT") {
        return Ok(p);
    }
    let ports = list_flipper_ports()?;
    match ports.len() {
        0 => Err(anyhow!(
            "no Flipper serial ports found (is it connected and the FAP running?)"
        )),
        1 => Ok(ports.into_iter().next().unwrap()),
        _ => Ok(ports.into_iter().last().unwrap()), // sorted; highest = interface 1
    }
}

pub struct SerialTransport {
    port: Box<dyn SerialPort>,
}

impl SerialTransport {
    pub fn open(path: &str) -> Result<Self> {
        let port = serialport::new(path, 115_200)
            .timeout(Duration::from_millis(50))
            .open()?;
        Ok(SerialTransport { port })
    }
}

impl Transport for SerialTransport {
    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        self.port.write_all(buf)?;
        self.port.flush()?;
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self.port.read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == ErrorKind::TimedOut => Ok(0),
            Err(e) => Err(e.into()),
        }
    }
}
