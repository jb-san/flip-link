use anyhow::{anyhow, Context, Result};
use flip_proto::Value;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

pub const MAX_SUBGHZ_DURATION_US: u32 = 0x3fff_ffff;

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
}
