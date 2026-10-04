# strict

The MixOS strict-data format: the grammar of `*.conf.mix` and `*.data.mix`
files. A document is a JSON-shaped tree (nil, bool, number, string, list,
map) with bare keys, optional top-level braces, `#` / `--` comments and
trailing commas. It is loaded as data and never executed: anything that
could run (a variable, a call, a command substitution, interpolation, a
semicolon) is refused with a line-numbered error naming the construct.

The crate has no dependency on the Mix language. It provides:

- `parse` / `parse_file` to a `Value` tree;
- `from_str` / `from_value` / `from_file` into a `#[derive(Deserialize)]`
  type, and `to_string` / `to_string_pretty` / `to_value` back out;
- `encode` / `encode_pretty` for a `Value`, in text that reads back
  unchanged;
- `to_json` / `from_json` for consumers that work on `serde_json::Value`.

Build and test with `cargo test -p strict`. The grammar tests live in
`tests/grammar.rs`, the serde bridge in `tests/serde_bridge.rs`.
