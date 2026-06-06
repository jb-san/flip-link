//! `flip ir transmit` helpers: parse a timings file into REQ params.

use anyhow::{anyhow, Context, Result};
use flip_proto::Value;

/// Parse whitespace/newline-separated unsigned integers (microsecond timings).
/// Lines starting with `#` are comments. Returns the timings as a Vec<u64>.
pub fn parse_timings(text: &str) -> Result<Vec<u64>> {
    let mut out = Vec::new();
    for tok in text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(|l| l.split_whitespace())
    {
        let v: u64 = tok
            .parse()
            .map_err(|_| anyhow!("invalid timing '{tok}' (expected unsigned integer µs)"))?;
        out.push(v);
    }
    if out.is_empty() {
        return Err(anyhow!("no timings found"));
    }
    Ok(out)
}

/// Build the `ir.transmit` params Value from timings + carrier settings.
pub fn transmit_params(timings: Vec<u64>, frequency: u64, duty_permille: u64) -> Value {
    Value::Map(vec![
        ("frequency".to_string(), Value::U64(frequency)),
        ("duty_permille".to_string(), Value::U64(duty_permille)),
        (
            "timings".to_string(),
            Value::Array(timings.into_iter().map(Value::U64).collect()),
        ),
    ])
}

/// Read + parse a timings file path.
pub fn load_timings_file(path: &str) -> Result<Vec<u64>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {path}"))?;
    parse_timings(&text)
}

/// Decode a STREAM_DATA payload (little-endian i32 µs) into timings, appending
/// to `out`. Returns the number of samples decoded.
pub fn decode_stream_data(payload: &[u8], out: &mut Vec<u64>) -> usize {
    let mut count = 0;
    for chunk in payload.chunks_exact(4) {
        let v = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        out.push(v.max(0) as u64);
        count += 1;
    }
    count
}

/// Render captured timings as the whitespace file format `ir transmit` reads:
/// 12 per line.
pub fn format_timings(timings: &[u64]) -> String {
    let mut s = String::new();
    for (i, t) in timings.iter().enumerate() {
        if i > 0 {
            s.push(if i % 12 == 0 { '\n' } else { ' ' });
        }
        s.push_str(&t.to_string());
    }
    s.push('\n');
    s
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    #[test]
    fn decodes_le_i32_samples() {
        let mut out = Vec::new();
        let payload = [0x10, 0x27, 0, 0, 0x2c, 0x01, 0, 0]; // 10000, 300
        assert_eq!(decode_stream_data(&payload, &mut out), 2);
        assert_eq!(out, vec![10000, 300]);
    }

    #[test]
    fn format_round_trips_through_parse() {
        let timings = vec![9000u64, 4500, 560, 560, 1690];
        let text = format_timings(&timings);
        assert_eq!(parse_timings(&text).unwrap(), timings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timings_with_comments_and_whitespace() {
        let text = "# an SOS\n9000 4500\n560 560\n560\n";
        assert_eq!(
            parse_timings(text).unwrap(),
            vec![9000, 4500, 560, 560, 560]
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_timings("hello").is_err());
        assert!(parse_timings("# only a comment").is_err());
    }

    #[test]
    fn builds_params() {
        let p = transmit_params(vec![560, 560], 38000, 330);
        assert_eq!(p.get("frequency"), Some(&Value::U64(38000)));
        assert_eq!(
            p.get("timings"),
            Some(&Value::Array(vec![Value::U64(560), Value::U64(560)]))
        );
    }
}
