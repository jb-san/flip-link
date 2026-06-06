//! Parse `key=value` CLI args into a `Value::Map`. Values are typed by shape:
//! integers -> U64/I64, true/false -> Bool, everything else -> Text.

use anyhow::{anyhow, Result};
use flip_proto::Value;

pub fn parse_params(args: &[String]) -> Result<Value> {
    let mut pairs = Vec::new();
    for arg in args {
        let (k, v) = arg
            .split_once('=')
            .ok_or_else(|| anyhow!("param '{arg}' is not key=value"))?;
        pairs.push((k.to_string(), parse_value(v)));
    }
    Ok(Value::Map(pairs))
}

fn parse_value(s: &str) -> Value {
    if s == "true" {
        return Value::Bool(true);
    }
    if s == "false" {
        return Value::Bool(false);
    }
    if let Ok(u) = s.parse::<u64>() {
        return Value::U64(u);
    }
    if let Ok(i) = s.parse::<i64>() {
        return Value::I64(i);
    }
    Value::Text(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typed_pairs() {
        let v = parse_params(&[
            "msg=hi".to_string(),
            "n=5".to_string(),
            "neg=-3".to_string(),
            "flag=true".to_string(),
        ])
        .unwrap();
        assert_eq!(
            v,
            Value::Map(vec![
                ("msg".to_string(), Value::Text("hi".to_string())),
                ("n".to_string(), Value::U64(5)),
                ("neg".to_string(), Value::I64(-3)),
                ("flag".to_string(), Value::Bool(true)),
            ])
        );
    }

    #[test]
    fn rejects_bare_arg() {
        assert!(parse_params(&["nope".to_string()]).is_err());
    }
}
