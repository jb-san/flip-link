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
