// SPDX-License-Identifier: MIT OR Apache-2.0

//! The config-file loader: the re-exported strict-data entry points behave
//! as a config reader expects, and a missing file is an error, not a panic.

use std::path::{Path, PathBuf};

use config::{ErrorKind, Value};

#[test]
fn parse_accepts_a_minimal_map() {
    let value = config::parse("name: \"alpha\"\npriority: 2\n").unwrap();
    let Value::Map(map) = &value else { panic!("expected map") };
    assert_eq!(map.get("name"), Some(&Value::String("alpha".into())));
    assert_eq!(map.get("priority"), Some(&Value::Number(2.0)));
}

#[test]
fn parse_refuses_an_executable_construct() {
    let error = config::parse("name: \"hi ${user}\"\n").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Violation);
    assert!(error.to_string().contains("Strict-data violation"), "got: {error}");
}

#[test]
fn parse_file_reads_a_fixture() {
    let value = config::parse_file(&manifest_dir().join("tests/fixtures/sample.conf.mix")).unwrap();
    assert_eq!(value.get("name").and_then(Value::as_str), Some("sample"));
    assert_eq!(value.get("priority").and_then(Value::as_f64), Some(3.0));
    assert_eq!(
        value.get("tags").and_then(Value::as_list).map(<[Value]>::len),
        Some(2)
    );
    assert_eq!(
        value
            .get("limits")
            .and_then(|limits| limits.get("connections"))
            .and_then(Value::as_f64),
        Some(100.0)
    );
}

#[test]
fn parse_file_missing_is_an_io_error_naming_the_path() {
    let error = config::parse_file(Path::new("/nonexistent/mixos/sample.conf.mix")).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Io);
    assert!(error.to_string().contains("/nonexistent/mixos/sample.conf.mix"), "got: {error}");
}

#[test]
fn a_value_tree_encodes_and_reads_back() {
    let mut root = config::Map::new();
    root.insert("scheme".into(), Value::String("dusk".into()));
    root.insert(
        "panels".into(),
        Value::Map(config::Map::from_iter([(
            "top".to_owned(),
            Value::List(vec![Value::String("clock".into())]),
        )])),
    );
    let value = Value::Map(root);
    let encoded = value.encode_pretty().unwrap();
    assert_eq!(config::parse(&encoded).unwrap(), value);
}

/// The manifest directory cargo exports into the test process, so a binary
/// built in one worktree and run in another still finds this tree's
/// fixtures; the compile-time value when run outside cargo.
fn manifest_dir() -> PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}
