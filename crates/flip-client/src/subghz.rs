use anyhow::{anyhow, Context, Result};
use flip_proto::Value;
use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

pub const MAX_SUBGHZ_DURATION_US: u32 = 0x3fff_ffff;
pub const MAX_LINK_PROBE_BYTES: usize = 60;
pub const MIN_LINK_PROBE_TIMEOUT_MS: u64 = 100;
pub const MAX_LINK_PROBE_TIMEOUT_MS: u64 = 5_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubGhzLinkProbeResult {
    pub written: u64,
    pub read: u64,
    pub callbacks: u64,
    pub rx_preview: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubGhzPreset {
    Ook270,
    Ook650,
    FskDev238,
    FskDev476,
    Msk99_97,
    Gfsk9_99,
}

impl SubGhzPreset {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ook270 => "ook270",
            Self::Ook650 => "ook650",
            Self::FskDev238 => "2fsk_dev238",
            Self::FskDev476 => "2fsk_dev476",
            Self::Msk99_97 => "msk99_97",
            Self::Gfsk9_99 => "gfsk9_99",
        }
    }
}

impl fmt::Display for SubGhzPreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SubGhzPreset {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "ook270" => Ok(Self::Ook270),
            "ook650" => Ok(Self::Ook650),
            "2fsk_dev238" => Ok(Self::FskDev238),
            "2fsk_dev476" => Ok(Self::FskDev476),
            "msk99_97" => Ok(Self::Msk99_97),
            "gfsk9_99" => Ok(Self::Gfsk9_99),
            _ => Err(anyhow!("unknown Sub-GHz preset '{value}'")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubGhzEdge {
    pub level: bool,
    pub duration_us: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubGhzSignal {
    pub frequency: u32,
    pub preset: SubGhzPreset,
    pub edges: Vec<SubGhzEdge>,
}

impl SubGhzSignal {
    pub fn parse(text: &str) -> Result<Self> {
        let mut frequency = None;
        let mut preset = None;
        let mut edges = Vec::new();

        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Some(comment) = trimmed.strip_prefix('#') {
                let directive = comment.trim();
                if let Some(rest) = directive.strip_prefix("frequency=") {
                    frequency = Some(parse_u32("frequency", rest)?);
                } else if let Some(rest) = directive.strip_prefix("preset=") {
                    let token = rest
                        .split_whitespace()
                        .next()
                        .ok_or_else(|| anyhow!("missing preset value"))?;
                    preset = Some(token.parse()?);
                }
                continue;
            }

            let fields = trimmed.split_whitespace().collect::<Vec<_>>();
            if fields.len() != 2 {
                return Err(anyhow!("invalid Sub-GHz edge line '{trimmed}'"));
            }
            let level = match fields[0] {
                "0" => false,
                "1" => true,
                other => return Err(anyhow!("invalid Sub-GHz level '{other}'")),
            };
            let duration_us = fields[1]
                .parse::<u32>()
                .map_err(|_| anyhow!("invalid Sub-GHz duration '{}'", fields[1]))?;
            if duration_us == 0 || duration_us > MAX_SUBGHZ_DURATION_US {
                return Err(anyhow!("Sub-GHz duration out of range: {duration_us}"));
            }
            edges.push(SubGhzEdge { level, duration_us });
        }

        let frequency = frequency.ok_or_else(|| anyhow!("Sub-GHz frequency missing"))?;
        let preset = preset.ok_or_else(|| anyhow!("Sub-GHz preset missing"))?;
        if edges.is_empty() {
            return Err(anyhow!("no Sub-GHz edges found"));
        }
        Ok(Self {
            frequency,
            preset,
            edges,
        })
    }

    pub fn to_file_string(&self) -> String {
        let mut out = format!(
            "# format=flip-subghz-raw-v1\n# frequency={}\n# preset={}\n",
            self.frequency, self.preset
        );
        for edge in &self.edges {
            out.push_str(if edge.level { "1 " } else { "0 " });
            out.push_str(&edge.duration_us.to_string());
            out.push('\n');
        }
        out
    }

    pub fn read_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        Self::parse(&text)
    }

    pub fn write_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        std::fs::write(path, self.to_file_string())
            .with_context(|| format!("write {}", path.display()))
    }

    pub fn capture_params(frequency: u32, preset: SubGhzPreset) -> Value {
        Value::Map(vec![
            ("frequency".into(), Value::U64(frequency as u64)),
            ("preset".into(), Value::Text(preset.to_string())),
        ])
    }

    pub fn to_transmit_params(&self, repeat: u32) -> Value {
        Value::Map(vec![
            ("frequency".into(), Value::U64(self.frequency as u64)),
            ("preset".into(), Value::Text(self.preset.to_string())),
            ("repeat".into(), Value::U64(repeat as u64)),
            (
                "edges".into(),
                Value::Array(self.edges.iter().map(edge_to_value).collect()),
            ),
        ])
    }
}

pub fn parse_probe_hex(input: &str) -> Result<Vec<u8>> {
    let mut cleaned = String::new();
    for part in input.split_whitespace() {
        let part = part.strip_prefix("0x").unwrap_or(part);
        cleaned.push_str(part);
    }
    if !cleaned
        .as_bytes()
        .iter()
        .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(anyhow!("hex payload contains non-hex digits"));
    }
    if !cleaned.len().is_multiple_of(2) {
        return Err(anyhow!("hex payload must contain an even number of digits"));
    }

    let mut out = Vec::with_capacity(cleaned.len() / 2);
    for index in (0..cleaned.len()).step_by(2) {
        let byte = u8::from_str_radix(&cleaned[index..index + 2], 16)
            .map_err(|_| anyhow!("invalid hex byte '{}'", &cleaned[index..index + 2]))?;
        out.push(byte);
    }
    validate_probe_payload(&out)?;
    Ok(out)
}

pub(crate) fn link_probe_params(
    frequency: u32,
    payload: &[u8],
    timeout: Duration,
) -> Result<Value> {
    validate_probe_payload(payload)?;
    if timeout < Duration::from_millis(MIN_LINK_PROBE_TIMEOUT_MS)
        || timeout > Duration::from_millis(MAX_LINK_PROBE_TIMEOUT_MS)
        || !timeout.subsec_nanos().is_multiple_of(1_000_000)
    {
        return Err(anyhow!(
            "link probe timeout must be whole milliseconds in {}..={} ms",
            MIN_LINK_PROBE_TIMEOUT_MS,
            MAX_LINK_PROBE_TIMEOUT_MS
        ));
    }
    let timeout_ms = timeout.as_millis();
    Ok(Value::Map(vec![
        ("frequency".into(), Value::U64(frequency as u64)),
        ("payload".into(), Value::Bytes(payload.to_vec())),
        ("timeout_ms".into(), Value::U64(timeout_ms as u64)),
    ]))
}

pub(crate) fn link_probe_result(value: &Value) -> Result<SubGhzLinkProbeResult> {
    Ok(SubGhzLinkProbeResult {
        written: required_u64(value, "written")?,
        read: required_u64(value, "read")?,
        callbacks: required_u64(value, "callbacks")?,
        rx_preview: match value.get("rx_preview") {
            Some(Value::Bytes(bytes)) if bytes.len() <= MAX_LINK_PROBE_BYTES => bytes.clone(),
            Some(Value::Bytes(bytes)) => {
                return Err(anyhow!(
                    "link probe response rx_preview too large: {} bytes (max {})",
                    bytes.len(),
                    MAX_LINK_PROBE_BYTES
                ));
            }
            _ => return Err(anyhow!("link probe response missing rx_preview bytes")),
        },
    })
}

fn validate_probe_payload(payload: &[u8]) -> Result<()> {
    if payload.is_empty() {
        return Err(anyhow!("link probe payload is empty"));
    }
    if payload.len() > MAX_LINK_PROBE_BYTES {
        return Err(anyhow!(
            "link probe payload too large: {} bytes (max {})",
            payload.len(),
            MAX_LINK_PROBE_BYTES
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, key: &str) -> Result<u64> {
    match value.get(key) {
        Some(Value::U64(n)) => Ok(*n),
        _ => Err(anyhow!("link probe response missing numeric {key} field")),
    }
}

fn edge_to_value(edge: &SubGhzEdge) -> Value {
    Value::Map(vec![
        ("level".into(), Value::Bool(edge.level)),
        ("duration_us".into(), Value::U64(edge.duration_us as u64)),
    ])
}

fn parse_u32(name: &str, rest: &str) -> Result<u32> {
    let token = rest
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("missing {name} value"))?;
    token
        .parse::<u32>()
        .map_err(|_| anyhow!("invalid {name} value '{token}'"))
}

pub(crate) fn decode_stream_data(payload: &[u8], out: &mut Vec<SubGhzEdge>) -> usize {
    let mut count = 0;
    for chunk in payload.chunks_exact(5) {
        let duration_us = u32::from_le_bytes([chunk[1], chunk[2], chunk[3], chunk[4]]);
        if duration_us > 0 {
            out.push(SubGhzEdge {
                level: chunk[0] != 0,
                duration_us,
            });
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_parse_and_display_round_trip() {
        for (text, preset) in [
            ("ook270", SubGhzPreset::Ook270),
            ("ook650", SubGhzPreset::Ook650),
            ("2fsk_dev238", SubGhzPreset::FskDev238),
            ("2fsk_dev476", SubGhzPreset::FskDev476),
            ("msk99_97", SubGhzPreset::Msk99_97),
            ("gfsk9_99", SubGhzPreset::Gfsk9_99),
        ] {
            assert_eq!(text.parse::<SubGhzPreset>().unwrap(), preset);
            assert_eq!(preset.to_string(), text);
        }
        assert!("bad".parse::<SubGhzPreset>().is_err());
    }

    #[test]
    fn raw_file_round_trips() {
        let signal = SubGhzSignal {
            frequency: 433_920_000,
            preset: SubGhzPreset::Ook650,
            edges: vec![
                SubGhzEdge {
                    level: true,
                    duration_us: 9000,
                },
                SubGhzEdge {
                    level: false,
                    duration_us: 4500,
                },
            ],
        };

        assert_eq!(
            SubGhzSignal::parse(&signal.to_file_string()).unwrap(),
            signal
        );
    }

    #[test]
    fn parse_rejects_missing_metadata_and_bad_edges() {
        assert!(SubGhzSignal::parse("1 100\n").is_err());
        assert!(SubGhzSignal::parse("# frequency=433920000\n1 100\n").is_err());
        assert!(SubGhzSignal::parse("# frequency=433920000\n# preset=ook650\n2 100\n").is_err());
        assert!(SubGhzSignal::parse("# frequency=433920000\n# preset=ook650\n1 0\n").is_err());
    }

    #[test]
    fn decodes_five_byte_stream_records() {
        let mut out = Vec::new();
        let payload = [1, 0x28, 0x23, 0, 0, 0, 0x94, 0x11, 0, 0];

        assert_eq!(decode_stream_data(&payload, &mut out), 2);
        assert_eq!(
            out,
            vec![
                SubGhzEdge {
                    level: true,
                    duration_us: 9000,
                },
                SubGhzEdge {
                    level: false,
                    duration_us: 4500,
                },
            ]
        );
    }

    #[test]
    fn transmit_params_include_frequency_preset_edges() {
        let signal = SubGhzSignal {
            frequency: 433_920_000,
            preset: SubGhzPreset::Ook650,
            edges: vec![SubGhzEdge {
                level: true,
                duration_us: 9000,
            }],
        };

        let params = signal.to_transmit_params(3);
        assert_eq!(
            params.get("frequency"),
            Some(&flip_proto::Value::U64(433_920_000))
        );
        assert_eq!(
            params.get("preset"),
            Some(&flip_proto::Value::Text("ook650".into()))
        );
        assert_eq!(params.get("repeat"), Some(&flip_proto::Value::U64(3)));
    }

    #[test]
    fn link_probe_params_validate_payload_and_timeout() {
        let params = link_probe_params(433_920_000, b"hello", Duration::from_millis(250)).unwrap();

        assert_eq!(
            params.get("frequency"),
            Some(&flip_proto::Value::U64(433_920_000))
        );
        assert_eq!(
            params.get("payload"),
            Some(&flip_proto::Value::Bytes(b"hello".to_vec()))
        );
        assert_eq!(params.get("timeout_ms"), Some(&flip_proto::Value::U64(250)));

        assert_eq!(
            link_probe_params(
                433_920_000,
                b"hello",
                Duration::from_millis(MIN_LINK_PROBE_TIMEOUT_MS)
            )
            .unwrap()
            .get("timeout_ms"),
            Some(&flip_proto::Value::U64(MIN_LINK_PROBE_TIMEOUT_MS))
        );
        assert_eq!(
            link_probe_params(
                433_920_000,
                b"hello",
                Duration::from_millis(MAX_LINK_PROBE_TIMEOUT_MS)
            )
            .unwrap()
            .get("timeout_ms"),
            Some(&flip_proto::Value::U64(MAX_LINK_PROBE_TIMEOUT_MS))
        );
        assert_eq!(
            link_probe_params(
                433_920_000,
                &[0xaa; MAX_LINK_PROBE_BYTES],
                Duration::from_millis(250)
            )
            .unwrap()
            .get("payload"),
            Some(&flip_proto::Value::Bytes(vec![0xaa; MAX_LINK_PROBE_BYTES]))
        );

        assert!(link_probe_params(433_920_000, b"", Duration::from_millis(250)).is_err());
        assert!(link_probe_params(433_920_000, b"hello", Duration::ZERO).is_err());
        assert!(link_probe_params(
            433_920_000,
            b"hello",
            Duration::from_millis(MIN_LINK_PROBE_TIMEOUT_MS - 1)
        )
        .is_err());
        assert!(link_probe_params(433_920_000, b"hello", Duration::from_nanos(1)).is_err());
        assert!(link_probe_params(
            433_920_000,
            b"hello",
            Duration::from_millis(250) + Duration::from_nanos(1)
        )
        .is_err());
        assert!(link_probe_params(433_920_000, b"hello", Duration::from_millis(5001)).is_err());
        assert!(link_probe_params(
            433_920_000,
            b"hello",
            Duration::from_millis(5000) + Duration::from_nanos(1)
        )
        .is_err());
        assert!(link_probe_params(
            433_920_000,
            &[0xaa; MAX_LINK_PROBE_BYTES + 1],
            Duration::from_millis(250)
        )
        .is_err());
    }

    #[test]
    fn parses_link_probe_result_map() {
        let value = flip_proto::Value::Map(vec![
            ("written".into(), flip_proto::Value::U64(5)),
            ("read".into(), flip_proto::Value::U64(2)),
            ("callbacks".into(), flip_proto::Value::U64(1)),
            (
                "rx_preview".into(),
                flip_proto::Value::Bytes(vec![0xab, 0xcd]),
            ),
        ]);

        assert_eq!(
            link_probe_result(&value).unwrap(),
            SubGhzLinkProbeResult {
                written: 5,
                read: 2,
                callbacks: 1,
                rx_preview: vec![0xab, 0xcd],
            }
        );
    }

    #[test]
    fn link_probe_result_rejects_wrong_shape() {
        assert!(link_probe_result(&flip_proto::Value::Map(vec![
            ("read".into(), flip_proto::Value::U64(2)),
            ("callbacks".into(), flip_proto::Value::U64(1)),
            ("rx_preview".into(), flip_proto::Value::Bytes(vec![0xab])),
        ]))
        .is_err());

        assert!(link_probe_result(&flip_proto::Value::Map(vec![
            ("written".into(), flip_proto::Value::Text("5".into())),
            ("read".into(), flip_proto::Value::U64(2)),
            ("callbacks".into(), flip_proto::Value::U64(1)),
            ("rx_preview".into(), flip_proto::Value::Bytes(vec![0xab])),
        ]))
        .is_err());

        assert!(link_probe_result(&flip_proto::Value::Map(vec![
            ("written".into(), flip_proto::Value::U64(5)),
            ("read".into(), flip_proto::Value::U64(2)),
            ("callbacks".into(), flip_proto::Value::U64(1)),
            ("rx_preview".into(), flip_proto::Value::Text("ab".into())),
        ]))
        .is_err());

        assert!(link_probe_result(&flip_proto::Value::Map(vec![
            ("written".into(), flip_proto::Value::U64(5)),
            ("read".into(), flip_proto::Value::U64(2)),
            ("callbacks".into(), flip_proto::Value::U64(1)),
            (
                "rx_preview".into(),
                flip_proto::Value::Bytes(vec![0xab; MAX_LINK_PROBE_BYTES + 1]),
            ),
        ]))
        .is_err());
    }

    #[test]
    fn parses_hex_probe_payload() {
        assert_eq!(parse_probe_hex("0x6865 6c6c6f").unwrap(), b"hello");
        assert!(parse_probe_hex("abc").is_err());
        assert!(parse_probe_hex("zz").is_err());
        let non_ascii = std::panic::catch_unwind(|| parse_probe_hex("€a"));
        assert!(non_ascii.is_ok());
        assert!(non_ascii.unwrap().is_err());
    }
}
