//! The document envelope: a `---` front-matter block of `key: value`
//! headers followed by the body.
//!
//! The parser is strict. Every non-empty header line must be `key: value`
//! with no leading whitespace and no whitespace inside the key, and a value
//! that starts with `[` or `{` must be valid JSON. Anything else is an
//! error naming the offending line, so a YAML list, an indented
//! continuation or a bare scalar never slips through as a header.

use std::collections::BTreeMap;

/// Ordered headers plus the trimmed body.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Envelope {
    headers: BTreeMap<String, String>,
    pub(crate) body: String,
}

impl Envelope {
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.headers.get(key).map(String::as_str)
    }
}

/// Parse an envelope. The error is a one-line message for a diagnostic.
pub(crate) fn parse(raw: &str) -> Result<Envelope, String> {
    let content = raw.strip_prefix("---\n").ok_or_else(|| {
        let head: String = raw.chars().take(40).collect();
        format!("document must start with a '---' header line, got {head:?}")
    })?;
    let (header_block, body) = match content.split_once("\n---\n") {
        Some((headers, body)) => (headers, body),
        None => {
            let headers = content
                .strip_suffix("\n---\n")
                .or_else(|| content.strip_suffix("\n---"))
                .or_else(|| content.strip_suffix("---\n"))
                .or_else(|| content.strip_suffix("---"))
                .unwrap_or(content);
            (headers, "")
        }
    };
    let mut headers = BTreeMap::new();
    for (index, line) in header_block.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let number = index + 1;
        let Some((key, value)) = line.split_once(": ") else {
            return Err(format!("header line {number} is not `key: value`: {line:?}"));
        };
        let key = key.trim();
        if line.starts_with(char::is_whitespace) || key.is_empty() || key.contains(char::is_whitespace) {
            return Err(format!("header line {number} is not `key: value`: {line:?}"));
        }
        let value = value.trim();
        if value.starts_with(['[', '{']) && serde_json::from_str::<serde_json::Value>(value).is_err() {
            return Err(format!("header {key} on line {number} is not valid JSON: {value:?}"));
        }
        headers.insert(key.to_owned(), value.to_owned());
    }
    Ok(Envelope { headers, body: body.trim_end().to_owned() })
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn headers_and_body_split_at_the_second_rule() {
        let envelope = parse("---\nscene: 1\nname: x\n\nwindow: {\"kind\":\"edge\"}\n---\nbody\nmore\n\n").unwrap();
        assert_eq!(envelope.get("scene"), Some("1"));
        assert_eq!(envelope.get("name"), Some("x"));
        assert_eq!(envelope.get("window"), Some("{\"kind\":\"edge\"}"));
        assert_eq!(envelope.get("missing"), None);
        assert_eq!(envelope.body, "body\nmore");
    }

    #[test]
    fn a_header_only_document_has_an_empty_body() {
        for raw in ["---\nscene: 1\n---\n", "---\nscene: 1\n---", "---\nscene: 1"] {
            let envelope = parse(raw).unwrap();
            assert_eq!(envelope.get("scene"), Some("1"), "{raw:?}");
            assert_eq!(envelope.body, "", "{raw:?}");
        }
    }

    #[test]
    fn non_conforming_header_lines_are_refused() {
        for raw in [
            "not an envelope",
            "---\n- item\n---\n",
            "---\n  indented: 1\n---\n",
            "---\nbad key: 1\n---\n",
            "---\nscalar\n---\n",
            "---\nlist: [a, b]\n---\n",
            "---\nmap: {kind: edge}\n---\n",
        ] {
            assert!(parse(raw).is_err(), "{raw:?}");
        }
    }
}
