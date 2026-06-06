use anyhow::{Context, Result};
use flip_proto::Value;
use std::path::Path;

pub const DEFAULT_FREQUENCY: u32 = 38_000;
pub const DEFAULT_DUTY_PERMILLE: u32 = 330;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrSignal {
    pub frequency: u32,
    pub duty_permille: u32,
    pub timings: Vec<u64>,
}

impl IrSignal {
    pub fn from_capture(raw: Vec<u64>) -> Self {
        let timings = raw.into_iter().skip(1).collect();
        Self {
            frequency: DEFAULT_FREQUENCY,
            duty_permille: DEFAULT_DUTY_PERMILLE,
            timings,
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut frequency = DEFAULT_FREQUENCY;
        let mut duty_permille = DEFAULT_DUTY_PERMILLE;
        let mut timings = Vec::new();

        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Some(comment) = trimmed.strip_prefix('#') {
                let directive = comment.trim();
                if let Some(rest) = directive.strip_prefix("freq=") {
                    frequency = parse_directive_u32("freq", rest)?;
                } else if let Some(rest) = directive.strip_prefix("duty=") {
                    duty_permille = parse_directive_u32("duty", rest)?;
                }
                continue;
            }

            for token in trimmed.split_whitespace() {
                let timing = token.parse::<u64>().map_err(|_| {
                    anyhow::anyhow!("invalid timing '{token}' (expected unsigned integer us)")
                })?;
                timings.push(timing);
            }
        }

        if timings.is_empty() {
            return Err(anyhow::anyhow!("no timings found"));
        }

        Ok(Self {
            frequency,
            duty_permille,
            timings,
        })
    }

    pub fn to_file_string(&self) -> String {
        let mut out = format!("# freq={}\n# duty={}\n", self.frequency, self.duty_permille);
        for (i, timing) in self.timings.iter().enumerate() {
            if i > 0 {
                out.push(if i % 12 == 0 { '\n' } else { ' ' });
            }
            out.push_str(&timing.to_string());
        }
        out.push('\n');
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

    pub fn to_transmit_params(&self) -> Value {
        Value::Map(vec![
            ("frequency".to_string(), Value::U64(self.frequency as u64)),
            (
                "duty_permille".to_string(),
                Value::U64(self.duty_permille as u64),
            ),
            (
                "timings".to_string(),
                Value::Array(self.timings.iter().copied().map(Value::U64).collect()),
            ),
        ])
    }
}

fn parse_directive_u32(name: &str, rest: &str) -> Result<u32> {
    let token = rest
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing {name} value"))?;
    token
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("invalid {name} value '{token}'"))
}

pub fn decode_stream_data(payload: &[u8], out: &mut Vec<u64>) -> usize {
    let mut count = 0;
    for chunk in payload.chunks_exact(4) {
        let value = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        out.push(value.max(0) as u64);
        count += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_existing_plain_timings_file_uses_defaults() {
        let signal = IrSignal::parse("# old file\n9000 4500\n560 560\n").unwrap();

        assert_eq!(signal.frequency, DEFAULT_FREQUENCY);
        assert_eq!(signal.duty_permille, DEFAULT_DUTY_PERMILLE);
        assert_eq!(signal.timings, vec![9000, 4500, 560, 560]);
    }

    #[test]
    fn parse_directives_with_trailing_comment_text() {
        let signal = IrSignal::parse(
            "# freq=40000      carrier in Hz\n# duty=250        duty in permille\n900 450\n",
        )
        .unwrap();

        assert_eq!(signal.frequency, 40_000);
        assert_eq!(signal.duty_permille, 250);
        assert_eq!(signal.timings, vec![900, 450]);
    }

    #[test]
    fn rejects_invalid_timing_or_directive_value() {
        assert!(IrSignal::parse("9000 hello").is_err());
        assert!(IrSignal::parse("# freq=nope\n9000").is_err());
        assert!(IrSignal::parse("# duty=nope\n9000").is_err());
        assert!(IrSignal::parse("# only comments\n").is_err());
    }

    #[test]
    fn write_emits_directives_and_twelve_timings_per_line() {
        let signal = IrSignal {
            frequency: 38_000,
            duty_permille: 330,
            timings: (1..=13).collect(),
        };

        assert_eq!(
            signal.to_file_string(),
            "# freq=38000\n# duty=330\n1 2 3 4 5 6 7 8 9 10 11 12\n13\n"
        );
    }

    #[test]
    fn file_format_round_trips() {
        let signal = IrSignal {
            frequency: 36_000,
            duty_permille: 400,
            timings: vec![9000, 4500, 560, 1690],
        };

        assert_eq!(IrSignal::parse(&signal.to_file_string()).unwrap(), signal);
    }

    #[test]
    fn from_capture_drops_leading_idle() {
        let signal = IrSignal::from_capture(vec![123_456, 9000, 4500, 560]);

        assert_eq!(signal.frequency, DEFAULT_FREQUENCY);
        assert_eq!(signal.duty_permille, DEFAULT_DUTY_PERMILLE);
        assert_eq!(signal.timings, vec![9000, 4500, 560]);
    }

    #[test]
    fn empty_capture_returns_empty_default_signal() {
        let signal = IrSignal::from_capture(Vec::new());

        assert_eq!(signal.frequency, DEFAULT_FREQUENCY);
        assert_eq!(signal.duty_permille, DEFAULT_DUTY_PERMILLE);
        assert!(signal.timings.is_empty());
    }

    #[test]
    fn decodes_le_i32_stream_samples() {
        let mut out = Vec::new();
        let payload = [0x10, 0x27, 0, 0, 0x2c, 0x01, 0, 0];

        assert_eq!(decode_stream_data(&payload, &mut out), 2);
        assert_eq!(out, vec![10_000, 300]);
    }

    #[test]
    fn decode_stream_data_clamps_negative_samples_to_zero() {
        let mut out = Vec::new();
        let payload = (-42i32).to_le_bytes();

        assert_eq!(decode_stream_data(&payload, &mut out), 1);
        assert_eq!(out, vec![0]);
    }

    #[test]
    fn decode_stream_data_ignores_partial_trailing_bytes() {
        let mut out = Vec::new();
        let payload = [0x2c, 0x01, 0, 0, 0xff, 0xee];

        assert_eq!(decode_stream_data(&payload, &mut out), 1);
        assert_eq!(out, vec![300]);
    }

    #[test]
    fn transmit_params_include_frequency_duty_and_timings() {
        let params = IrSignal {
            frequency: 40_000,
            duty_permille: 250,
            timings: vec![560, 1690],
        }
        .to_transmit_params();

        assert_eq!(params.get("frequency"), Some(&Value::U64(40_000)));
        assert_eq!(params.get("duty_permille"), Some(&Value::U64(250)));
        assert_eq!(
            params.get("timings"),
            Some(&Value::Array(vec![Value::U64(560), Value::U64(1690)]))
        );
    }
}
