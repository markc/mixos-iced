//! Writing a [`Value`] back out as strict-data text.
//!
//! Two layouts share one escaping rule. The compact form is a single line
//! (`{"name": "alpha", "port": 25}`); the pretty form puts one entry per
//! line at two spaces per level and keeps empty containers as `{}` / `[]`.
//! Both quote every string and every key, so a value that looks like a
//! keyword (`"true"`), an empty string, or a key with a dash survives the
//! trip back through [`crate::parse`]. Config files written by one layout
//! and read back must stay byte-identical when re-encoded with the same
//! layout.

use std::fmt::Write;

use crate::error::{Error, Result};
use crate::value::Value;

/// The compact, one-line strict-data text of `value`.
///
/// Fails on a non-finite number, which has no spelling that reads back.
pub fn encode(value: &Value) -> Result<String> {
    let mut out = String::new();
    write_value(value, &mut out, 0, false)?;
    Ok(out)
}

/// The indented, multi-line strict-data text of `value`: one map entry or
/// list element per line, two spaces per level, no trailing newline. It
/// parses to the same tree as the compact form.
pub fn encode_pretty(value: &Value) -> Result<String> {
    let mut out = String::new();
    write_value(value, &mut out, 0, true)?;
    Ok(out)
}

fn write_value(value: &Value, out: &mut String, indent: usize, pretty: bool) -> Result<()> {
    match value {
        Value::Nil => out.push_str("nil"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => write_number(*n, out)?,
        Value::String(s) => write_string(s, out),
        Value::List(items) => {
            if !pretty || items.is_empty() {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_value(item, out, indent, pretty)?;
                }
                out.push(']');
            } else {
                out.push_str("[\n");
                let inner = indent + 1;
                for (i, item) in items.iter().enumerate() {
                    push_indent(out, inner);
                    write_value(item, out, inner, pretty)?;
                    if i + 1 < items.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                push_indent(out, indent);
                out.push(']');
            }
        }
        Value::Map(entries) => {
            if !pretty || entries.is_empty() {
                out.push('{');
                for (i, (key, item)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_string(key, out);
                    out.push_str(": ");
                    write_value(item, out, indent, pretty)?;
                }
                out.push('}');
            } else {
                out.push_str("{\n");
                let inner = indent + 1;
                for (i, (key, item)) in entries.iter().enumerate() {
                    push_indent(out, inner);
                    write_string(key, out);
                    out.push_str(": ");
                    write_value(item, out, inner, pretty)?;
                    if i + 1 < entries.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                push_indent(out, indent);
                out.push('}');
            }
        }
    }
    Ok(())
}

/// Integers print without a fraction when the `i64` cast is exact; every
/// other finite value prints as Rust's shortest round-tripping `f64`. The
/// exactness check (not a mere "is integral" test) keeps 1e20, which
/// saturates the cast, on the float path.
fn write_number(n: f64, out: &mut String) -> Result<()> {
    if !n.is_finite() {
        return Err(Error::encode(format!(
            "non-finite number {n} has no strict-data representation"
        )));
    }
    let as_int = n as i64;
    if (as_int as f64) == n {
        let _ = write!(out, "{as_int}");
    } else {
        let _ = write!(out, "{n}");
    }
    Ok(())
}

fn push_indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

/// Quote `s` so the lexer reads it back unchanged: `\\ \" \n \t \r \e \$`,
/// any other control character as `\u{…}`, and a leading `\~` when the
/// string is `~` or starts with `~/` (the two shapes a double-quoted string
/// would otherwise expand to the home directory). Non-ASCII text is written
/// as itself.
fn write_string(s: &str, out: &mut String) {
    out.push('"');
    let escape_tilde = {
        let mut chars = s.chars();
        chars.next() == Some('~') && matches!(chars.next(), None | Some('/'))
    };
    if escape_tilde {
        out.push_str("\\~");
    }
    for ch in s.chars().skip(usize::from(escape_tilde)) {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\x1b' => out.push_str("\\e"),
            '$' => out.push_str("\\$"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:X}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Map;

    fn sample() -> Value {
        let mut m = Map::new();
        m.insert("name".into(), Value::String("alpha".into()));
        m.insert("port".into(), Value::Number(25.0));
        m.insert(
            "tags".into(),
            Value::List(vec![Value::String("a".into()), Value::String("b".into())]),
        );
        Value::Map(m)
    }

    #[test]
    fn compact_layout() {
        assert_eq!(
            encode(&sample()).unwrap(),
            r#"{"name": "alpha", "port": 25, "tags": ["a", "b"]}"#
        );
    }

    #[test]
    fn pretty_layout() {
        assert_eq!(
            encode_pretty(&sample()).unwrap(),
            "{\n  \"name\": \"alpha\",\n  \"port\": 25,\n  \"tags\": [\n    \"a\",\n    \"b\"\n  ]\n}"
        );
        let mut m = Map::new();
        m.insert("empty".into(), Value::List(vec![]));
        m.insert("none".into(), Value::Map(Map::new()));
        assert_eq!(
            encode_pretty(&Value::Map(m)).unwrap(),
            "{\n  \"empty\": [],\n  \"none\": {}\n}"
        );
    }

    #[test]
    fn sigils_and_controls_are_escaped() {
        let mut m = Map::new();
        m.insert("rx".into(), Value::String("ends-with$".into()));
        m.insert("win".into(), Value::String("a\\b".into()));
        assert_eq!(encode(&Value::Map(m)).unwrap(), r#"{"rx": "ends-with\$", "win": "a\\b"}"#);
        assert_eq!(encode(&Value::String("~/x\u{1}".into())).unwrap(), r#""\~/x\u{1}""#);
        assert_eq!(encode(&Value::String("~x".into())).unwrap(), r#""~x""#);
    }

    #[test]
    fn numbers() {
        for (n, text) in [
            (0.0, "0"),
            (-0.0, "0"),
            (42.0, "42"),
            (-42.0, "-42"),
            (0.75, "0.75"),
            (1e20, "100000000000000000000"),
            (1e-7, "0.0000001"),
        ] {
            assert_eq!(encode(&Value::Number(n)).unwrap(), text, "{n}");
        }
        for n in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            let err = encode(&Value::Number(n)).unwrap_err();
            assert_eq!(err.kind(), crate::ErrorKind::Encode);
            assert!(err.message().contains("non-finite"));
        }
    }
}
