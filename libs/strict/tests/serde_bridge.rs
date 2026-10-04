//! The serde bridge: typed structs in and out of strict data.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use strict::{ErrorKind, IndexMap, Value, from_str, from_value, to_string, to_string_pretty, to_value};

fn map(pairs: &[(&str, Value)]) -> Value {
    let mut m = IndexMap::new();
    for (k, v) in pairs {
        m.insert((*k).to_string(), v.clone());
    }
    Value::Map(m)
}

// --- untagged enums -----------------------------------------------------------

#[derive(Debug, PartialEq, Deserialize)]
#[serde(untagged)]
enum ListenSpec {
    One(String),
    Many(Vec<String>),
}

#[test]
fn untagged_arms() {
    let got: ListenSpec = from_value(&Value::String("127.0.0.1:53".into())).unwrap();
    assert_eq!(got, ListenSpec::One("127.0.0.1:53".into()));
    let v = Value::List(vec![Value::String("a:1".into()), Value::String("b:2".into())]);
    let got: ListenSpec = from_value(&v).unwrap();
    assert_eq!(got, ListenSpec::Many(vec!["a:1".into(), "b:2".into()]));
}

// --- unit-variant enums as strings --------------------------------------------

#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum Provider {
    #[serde(rename = "letsencrypt_prod")]
    Prod,
    #[serde(rename = "letsencrypt_staging")]
    Staging,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Challenge {
    Http01,
    Dns01,
}

#[test]
fn enum_renames() {
    let p: Provider = from_value(&Value::String("letsencrypt_prod".into())).unwrap();
    assert_eq!(p, Provider::Prod);
    let c: Challenge = from_value(&Value::String("dns01".into())).unwrap();
    assert_eq!(c, Challenge::Dns01);
    let err = from_value::<Provider>(&Value::String("letsencrypt".into())).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Deserialize);
    assert!(err.message().contains("unknown variant"), "{err}");
}

#[test]
fn unit_variant_round_trips_as_string() {
    let v = to_value(&Provider::Prod).unwrap();
    assert_eq!(v, Value::String("letsencrypt_prod".into()));
    let back: Provider = from_value(&v).unwrap();
    assert_eq!(back, Provider::Prod);
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum Shape {
    Unit,
    Newtype(u8),
    Tuple(u8, String),
    Struct { x: f64 },
}

#[test]
fn payload_variants_round_trip_as_single_key_maps() {
    for shape in [
        Shape::Unit,
        Shape::Newtype(3),
        Shape::Tuple(1, "two".into()),
        Shape::Struct { x: 1.5 },
    ] {
        let text = to_string(&shape).unwrap();
        let back: Shape = from_str(&format!("[{text}]")).map(|mut v: Vec<Shape>| v.remove(0)).unwrap();
        assert_eq!(back, shape, "{text}");
    }
}

// --- deny_unknown_fields -------------------------------------------------------

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerConfig {
    name: String,
    port: u16,
}

#[test]
fn deny_unknown_fields_sees_every_key() {
    let v = map(&[("name", Value::String("alpha".into())), ("port", Value::Number(8080.0))]);
    let got: PeerConfig = from_value(&v).unwrap();
    assert_eq!(got, PeerConfig { name: "alpha".into(), port: 8080 });
    let v = map(&[
        ("name", Value::String("alpha".into())),
        ("port", Value::Number(8080.0)),
        ("hub_port", Value::Number(9090.0)),
    ]);
    let err = from_value::<PeerConfig>(&v).unwrap_err();
    assert!(
        err.message().contains("unknown field") && err.message().contains("hub_port"),
        "{err}"
    );
}

// --- exact integers --------------------------------------------------------------

#[test]
fn integers_in_range() {
    assert_eq!(from_value::<u16>(&Value::Number(8080.0)).unwrap(), 8080);
    assert_eq!(from_value::<i64>(&Value::Number(-42.0)).unwrap(), -42);
    assert_eq!(from_value::<u64>(&Value::Number(9_007_199_254_740_992.0)).unwrap(), 1 << 53);
}

#[test]
fn integers_rejected_when_inexact() {
    let err = from_value::<u16>(&Value::Number(3.5)).unwrap_err();
    assert!(err.message().contains("not an integer"), "{err}");
    let err = from_value::<u64>(&Value::Number(9_007_199_254_740_994.0)).unwrap_err();
    assert!(err.message().contains("exceeds the range"), "{err}");
    let err = from_value::<u32>(&Value::Number(-1.0)).unwrap_err();
    assert!(err.message().contains("negative"), "{err}");
    let err = from_value::<u16>(&Value::Number(70000.0)).unwrap_err();
    assert!(err.message().contains("invalid value"), "{err}");
    let err = from_value::<i32>(&Value::String("7".into())).unwrap_err();
    assert_eq!(err.message(), "expected number, found string");
}

#[test]
fn serialize_rejects_integers_beyond_exact_range() {
    let err = to_value(&9_007_199_254_740_994_u64).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Serialize);
    assert!(err.message().contains("exceeds the range"), "{err}");
    assert_eq!(to_value(&(1u64 << 53)).unwrap(), Value::Number(9_007_199_254_740_992.0));
}

// --- options and defaults ----------------------------------------------------------

#[derive(Debug, PartialEq, Deserialize)]
struct OptHolder {
    #[serde(default)]
    maybe: Option<u32>,
    #[serde(default)]
    name: String,
}

#[test]
fn nil_is_none_and_missing_is_default() {
    assert_eq!(from_value::<Option<u32>>(&Value::Nil).unwrap(), None);
    let got: OptHolder = from_value(&map(&[("maybe", Value::Nil), ("name", Value::String("x".into()))])).unwrap();
    assert_eq!(got, OptHolder { maybe: None, name: "x".into() });
    let got: OptHolder = from_value(&map(&[])).unwrap();
    assert_eq!(got, OptHolder { maybe: None, name: String::new() });
    let got: OptHolder = from_value(&map(&[("maybe", Value::Number(7.0))])).unwrap();
    assert_eq!(got.maybe, Some(7));
    assert_eq!(to_value(&Option::<u32>::None).unwrap(), Value::Nil);
}

// --- end to end ------------------------------------------------------------------------

#[test]
fn from_str_top_level_body() {
    let got: PeerConfig = from_str("name: \"alpha\"\nport: 8080\n").unwrap();
    assert_eq!(got, PeerConfig { name: "alpha".into(), port: 8080 });
    let got: OptHolder = from_str("{}").unwrap();
    assert_eq!(got, OptHolder { maybe: None, name: String::new() });
}

#[test]
fn from_str_propagates_parse_errors_with_their_kind() {
    let err = from_str::<PeerConfig>("name: $x\n").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Violation);
    assert!(err.to_string().contains("Strict-data violation"), "{err}");
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Sample {
    name: String,
    port: u16,
    ratio: f64,
    enabled: bool,
    tags: Vec<String>,
    #[serde(default)]
    note: Option<String>,
}

#[test]
fn struct_round_trip_in_both_layouts() {
    let s = Sample {
        name: "~/path/$HOME\nsecond line $x".into(),
        port: 25,
        ratio: 1.5,
        enabled: true,
        tags: vec!["a".into(), "b".into()],
        note: Some("trailing $ and ~ mid-string".into()),
    };
    let compact = to_string(&s).unwrap();
    assert_eq!(
        compact,
        r#"{"name": "\~/path/\$HOME\nsecond line \$x", "port": 25, "ratio": 1.5, "enabled": true, "tags": ["a", "b"], "note": "trailing \$ and ~ mid-string"}"#
    );
    let back: Sample = from_str(&compact).unwrap();
    assert_eq!(s, back);
    let pretty = to_string_pretty(&s).unwrap();
    assert!(pretty.starts_with("{\n  \"name\": "), "{pretty}");
    let back: Sample = from_str(&pretty).unwrap();
    assert_eq!(s, back);
}

#[test]
fn nested_map_round_trip() {
    let mut m: BTreeMap<String, u32> = BTreeMap::new();
    m.insert("one".into(), 1);
    m.insert("two".into(), 2);
    let text = to_string(&m).unwrap();
    assert_eq!(text, r#"{"one": 1, "two": 2}"#);
    let back: BTreeMap<String, u32> = from_str(&text).unwrap();
    assert_eq!(m, back);
}

#[test]
fn integer_map_keys_become_strings_and_other_keys_are_refused() {
    let mut m: BTreeMap<u32, bool> = BTreeMap::new();
    m.insert(7, true);
    assert_eq!(to_string(&m).unwrap(), r#"{"7": true}"#);
    let mut bad: BTreeMap<bool, u32> = BTreeMap::new();
    bad.insert(true, 1);
    assert_eq!(to_value(&bad).unwrap_err().kind(), ErrorKind::Serialize);
}

#[test]
fn value_itself_round_trips_through_the_bridge() {
    let original = strict::parse("a: [1, nil, \"x\", {b: false}]\n").unwrap();
    assert_eq!(to_value(&original).unwrap(), original);
    assert_eq!(from_value::<Value>(&original).unwrap(), original);
    assert_eq!(strict::parse(&to_string(&original).unwrap()).unwrap(), original);
}

#[test]
fn bytes_have_no_representation() {
    assert_eq!(to_value(&serde_bytes_like(&[1, 2])).unwrap_err().kind(), ErrorKind::Serialize);
}

/// A type that serializes through `serialize_bytes`.
struct Bytes<'a>(&'a [u8]);

impl Serialize for Bytes<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.0)
    }
}

fn serde_bytes_like(b: &[u8]) -> Bytes<'_> {
    Bytes(b)
}
