//! The strict-data grammar: what is accepted, what is refused and how, and
//! the encode -> parse round trip.

use strict::{Error, ErrorKind, IndexMap, Value, parse};

fn p(src: &str) -> Value {
    parse(src).expect("strict-data parse should accept this fixture")
}

fn rejects(src: &str) -> Error {
    match parse(src) {
        Ok(v) => panic!("expected a refusal, got {v:?}"),
        Err(e) => e,
    }
}

fn assert_violation(err: &Error, construct: &str) {
    assert_eq!(err.kind(), ErrorKind::Violation, "{err}");
    assert!(
        err.message().contains(construct),
        "expected the violation to name {construct:?}, got {:?}",
        err.message()
    );
}

fn map(v: &Value) -> &IndexMap<String, Value> {
    v.as_map().unwrap_or_else(|| panic!("expected a map, got {v:?}"))
}

// --- Accept -----------------------------------------------------------------

#[test]
fn accepts_top_level_map_body_without_braces() {
    let v = p("name: \"alpha\"\nnumber: 42\n");
    let m = map(&v);
    assert_eq!(m.get("name"), Some(&Value::String("alpha".into())));
    assert_eq!(m.get("number"), Some(&Value::Number(42.0)));
}

#[test]
fn accepts_explicit_brace_map_at_top_level() {
    assert_eq!(map(&p("{ name: \"alpha\", n: 1 }")).len(), 2);
}

#[test]
fn accepts_top_level_list() {
    assert_eq!(
        p("[1, 2, 3]"),
        Value::List(vec![Value::Number(1.0), Value::Number(2.0), Value::Number(3.0)])
    );
}

#[test]
fn accepts_empty_input_as_empty_map() {
    assert!(map(&p("")).is_empty());
    assert!(map(&p("\n\n  \n")).is_empty());
    assert!(map(&p("# only a comment\n")).is_empty());
}

#[test]
fn accepts_all_scalar_types() {
    let v = p("s: \"text\"\nn: 1.25\nneg: -42\nt: true\nf: false\nz: nil\nw: word\n");
    let m = map(&v);
    assert_eq!(m.get("s"), Some(&Value::String("text".into())));
    assert_eq!(m.get("n"), Some(&Value::Number(1.25)));
    assert_eq!(m.get("neg"), Some(&Value::Number(-42.0)));
    assert_eq!(m.get("t"), Some(&Value::Bool(true)));
    assert_eq!(m.get("f"), Some(&Value::Bool(false)));
    assert_eq!(m.get("z"), Some(&Value::Nil));
    assert_eq!(m.get("w"), Some(&Value::String("word".into())));
}

#[test]
fn accepts_literal_heredocs_with_trailing_newline_semantics() {
    let v = p("plain: <<E\nhello\nE\ntrailing: <<E\nhello\n\nE\n");
    let m = map(&v);
    assert_eq!(m.get("plain"), Some(&Value::String("hello".into())));
    assert_eq!(m.get("trailing"), Some(&Value::String("hello\n".into())));
    let v = p("value: <<END\nbody\nEND\n");
    assert_eq!(v.encode().unwrap(), r#"{"value": "body"}"#);
}

#[test]
fn accepts_nested_lists_and_maps() {
    let v = p("outer: { inner: [1, [2, 3], { deep: \"value\" }] }\nflat: [{ a: 1 }, { b: 2 }]\n");
    let m = map(&v);
    assert!(matches!(m.get("outer"), Some(Value::Map(_))));
    assert_eq!(m.get("flat").and_then(Value::as_list).map(<[Value]>::len), Some(2));
}

#[test]
fn accepts_quoted_and_keyword_keys() {
    let v = p("{ \"with-dash\": 1, plain: 2, to: 3, on: 4, fn: 5, 'single': 6 }");
    let m = map(&v);
    for key in ["with-dash", "plain", "to", "on", "fn", "single"] {
        assert!(m.contains_key(key), "missing {key}");
    }
}

#[test]
fn accepts_trailing_commas() {
    assert_eq!(p("[1, 2, 3,]").as_list().map(<[Value]>::len), Some(3));
    let v = p("m: {a: 1, b: 2,}\nxs: [1, 2,]\n");
    assert!(map(&v).contains_key("m") && map(&v).contains_key("xs"));
    assert_eq!(map(&p("{\n  a: 1,\n  b: 2,\n  c: 3,\n}")).len(), 3);
}

#[test]
fn accepts_comma_separated_top_level_entries() {
    assert_eq!(map(&p("a: 1, b: 2\nc: 3")).len(), 3);
}

#[test]
fn accepts_value_on_the_line_after_a_top_level_key() {
    let v = p("items:\n  [1, 2]\nname:\n  \"x\"\n");
    assert_eq!(map(&v).get("name"), Some(&Value::String("x".into())));
}

#[test]
fn accepts_comments_everywhere() {
    let v = p("# head\na: 1 # trailing hash\nb: [ # inside\n  2, -- dash comment\n  3\n]\n-- tail\n");
    assert_eq!(map(&v).get("a"), Some(&Value::Number(1.0)));
    assert_eq!(
        map(&v).get("b"),
        Some(&Value::List(vec![Value::Number(2.0), Value::Number(3.0)]))
    );
}

#[test]
fn accepts_crlf_line_endings() {
    assert_eq!(map(&p("a: 1\r\nb: 2\r\n")).len(), 2);
}

#[test]
fn accepts_number_spellings() {
    let v = p("a: 1_000\nb: .5\nc: 2.5e3\nd: 0x1F\ne: 0o755\nf: 0b101\ng: -0.5\nh: 1e-3\n");
    let m = map(&v);
    assert_eq!(m.get("a"), Some(&Value::Number(1000.0)));
    assert_eq!(m.get("b"), Some(&Value::Number(0.5)));
    assert_eq!(m.get("c"), Some(&Value::Number(2500.0)));
    assert_eq!(m.get("d"), Some(&Value::Number(31.0)));
    assert_eq!(m.get("e"), Some(&Value::Number(493.0)));
    assert_eq!(m.get("f"), Some(&Value::Number(5.0)));
    assert_eq!(m.get("g"), Some(&Value::Number(-0.5)));
    assert_eq!(m.get("h"), Some(&Value::Number(0.001)));
}

#[test]
fn accepts_full_spec_fixture() {
    let v = p(include_str!("fixtures/example.spec.mix"));
    let m = map(&v);
    assert_eq!(m.get("name"), Some(&Value::String("phase-8d.4-fix".into())));
    assert_eq!(m.get("priority"), Some(&Value::Number(2.0)));
    assert_eq!(m.get("draft"), Some(&Value::Bool(false)));
    assert_eq!(m.get("deadline"), Some(&Value::Nil));
    assert_eq!(m.get("mode"), Some(&Value::String("staging".into())));
    assert_eq!(m.get("allow_list").and_then(Value::as_list).map(<[Value]>::len), Some(1));
    let review = m.get("review").and_then(Value::as_map).expect("review map");
    assert_eq!(review.get("reviewer"), Some(&Value::String("second-pair".into())));
    assert_eq!(review.get("rounds").and_then(Value::as_list).map(<[Value]>::len), Some(2));
}

// --- Refuse -----------------------------------------------------------------

#[test]
fn rejects_string_interpolation() {
    assert_violation(&rejects("greeting: \"hello ${name}\"\n"), "interpolation");
    assert_violation(&rejects("note: <<E\n${name}\nE\n"), "interpolation");
    assert_violation(&rejects("out: <<E\n$(ls)\nE\n"), "command substitution");
}

#[test]
fn rejects_home_expansion_but_keeps_escaped_and_mid_string_tildes() {
    assert_violation(&rejects("dir: \"~/x\"\n"), "`~`");
    assert_violation(&rejects("dir: \"~\"\n"), "`~`");
    assert_eq!(map(&p("t: \"\\~/x\"\n")).get("t"), Some(&Value::String("~/x".into())));
    assert_eq!(map(&p("t: \"~x\"\n")).get("t"), Some(&Value::String("~x".into())));
    assert_eq!(map(&p("t: \"a~/b\"\n")).get("t"), Some(&Value::String("a~/b".into())));
}

#[test]
fn rejects_variable_reference() {
    let e = rejects("x: $foo\n");
    assert_violation(&e, "variable reference");
    assert_eq!(e.to_string(), "Strict-data violation at line 1: variable reference `$foo` not allowed in data files. data files have no variable scope; inline the value");
}

#[test]
fn rejects_command_substitution() {
    assert_violation(&rejects("dir: $(pwd)\n"), "command substitution");
}

#[test]
fn rejects_function_call() {
    let e = rejects("stamp: time()\n");
    assert_violation(&e, "function call");
    assert_eq!((e.line(), e.column()), (Some(1), Some(8)));
}

#[test]
fn rejects_executable_words() {
    assert_violation(&rejects("result: send \"target\" ping\n"), "send");
    assert_violation(&rejects("out: sh \"ls\"\n"), "sh");
    assert_violation(&rejects("f: function($x) = $x + 1\n"), "function literal");
    assert_violation(&rejects("f: fn($x) = 1\n"), "function literal");
}

#[test]
fn rejects_parenthesised_expression() {
    assert_violation(&rejects("n: (1 + 2)\n"), "parenthesised");
}

#[test]
fn rejects_arithmetic_via_missing_separator() {
    let e = rejects("n: 1 + 2\n");
    assert_violation(&e, "missing separator");
    assert_violation(&rejects("value: \"a\" ..\n  \"b\"\n"), "missing separator");
}

#[test]
fn rejects_negation_of_non_number() {
    assert_violation(&rejects("flag: -true\n"), "unary minus");
}

#[test]
fn rejects_duplicate_keys() {
    let e = rejects("role: \"user\"\nrole: \"admin\"\n");
    assert_eq!(e.kind(), ErrorKind::DuplicateKey);
    assert_eq!(e.message(), "duplicate map key `role`");
    assert_eq!(e.line(), Some(2));
    assert_eq!(rejects("{ a: 1, a: 2 }").kind(), ErrorKind::DuplicateKey);
}

#[test]
fn rejects_top_level_scalar() {
    assert_violation(&rejects("42"), "non-identifier map key");
}

#[test]
fn rejects_trailing_content() {
    assert_violation(&rejects("[1] [2]"), "trailing content");
    assert_violation(&rejects("{a: 1} b: 2"), "trailing content");
}

#[test]
fn rejects_semicolons_everywhere() {
    for source in ["a: 1; b: 2", "[1; 2]", "{a: 1; b: 2}", "; a: 1", "a: 1;"] {
        let err = rejects(source);
        assert_eq!(err.kind(), ErrorKind::Violation, "{source:?}: {err}");
        assert!(err.to_string().contains("semicolon"), "{source:?}: {err}");
    }
}

#[test]
fn rejects_braced_map_entries_without_comma() {
    let err = rejects("top: {\n  a: 1\n  b: 2\n}\n");
    assert_violation(&err, "missing `,` after this map entry");
    assert_eq!(err.line(), Some(2), "anchored at the entry missing its comma");
    assert_violation(&rejects("xs: [1\n2]\n"), "missing `,` after this list item");
}

#[test]
fn unterminated_map_is_a_syntax_error_not_a_missing_comma() {
    let err = rejects("top: {\n  a: 1\n");
    assert_eq!(err.kind(), ErrorKind::Syntax);
    assert!(!err.to_string().contains("missing `,`"), "{err}");
    assert!(err.to_string().contains("expected `}`"), "{err}");
}

#[test]
fn lexer_errors_carry_a_position() {
    let err = rejects("a: 1\nb: \"open\n");
    assert_eq!(err.kind(), ErrorKind::Lex);
    assert_eq!((err.line(), err.column()), (Some(2), Some(4)));
    assert_eq!(rejects("mode: 0755").kind(), ErrorKind::Lex);
}

#[test]
fn deep_nesting_errors_cleanly() {
    for src in [
        format!("{}1{}", "[".repeat(10_000), "]".repeat(10_000)),
        format!("{}1{}", "{a:".repeat(10_000), "}".repeat(10_000)),
    ] {
        let err = rejects(&src);
        assert_eq!(err.kind(), ErrorKind::Depth);
        assert!(err.message().contains("nesting too deep"), "{err}");
    }
    p(&format!("{}1{}", "{a:".repeat(100), "}".repeat(100)));
}

// --- Escapes ----------------------------------------------------------------

fn text(src: &str) -> String {
    match map(&p(src)).get("t") {
        Some(Value::String(s)) => s.clone(),
        other => panic!("expected string t, got {other:?}"),
    }
}

#[test]
fn escaped_dollar_and_tilde_are_literal() {
    assert_eq!(text(r#"{t: "title \${name} here"}"#), "title ${name} here");
    assert_eq!(text(r#"{t: "dangling \${"}"#), "dangling ${");
    assert_eq!(text(r#"{t: "\~/Downloads"}"#), "~/Downloads");
    assert_eq!(text(r#"{t: "bare $x stays"}"#), "bare $x stays");
}

#[test]
fn unicode_escapes_decode() {
    assert_eq!(text(r#"{t: "q\u0041"}"#), "qA");
    assert_eq!(text(r#"{t: "a\u0001b"}"#), "a\u{1}b");
    assert_eq!(text(r#"{t: "\ud83d\ude00"}"#), "\u{1F600}");
    assert_eq!(text(r#"{t: "\u{1F600}"}"#), "\u{1F600}");
    assert_eq!(text(r#"{t: "C:\users"}"#), "C:\\users");
    assert_eq!(text(r#"{t: "\x41\x4"}"#), "A\\x4");
    assert_eq!(text(r#"{t: "\0\a\b\f\v"}"#), "\0\u{7}\u{8}\u{c}\u{b}");
}

#[test]
fn lone_surrogates_are_refused() {
    assert!(parse(r#"{t: "\ud83d"}"#).is_err(), "high surrogate alone");
    assert!(parse(r#"{t: "\ud83dx"}"#).is_err(), "high surrogate + non-escape");
    assert!(parse(r#"{t: "\ude00"}"#).is_err(), "lone low surrogate");
}

#[test]
fn single_quoted_strings_are_raw() {
    assert_eq!(text(r"{t: 'a\nb\'c\\d ${x} ~/e'}"), "a\\nb'c\\d ${x} ~/e");
}

// --- Round trip -------------------------------------------------------------

#[test]
fn round_trips_through_encode() {
    let mut inner = IndexMap::new();
    inner.insert("port".to_string(), Value::Number(7777.0));
    inner.insert("tls".to_string(), Value::Bool(true));
    inner.insert("note".to_string(), Value::String("hello world".to_string()));
    inner.insert("empty".to_string(), Value::String(String::new()));
    inner.insert("keyword_lookalike".to_string(), Value::String("true".to_string()));
    inner.insert(
        "with_specials".to_string(),
        Value::String("quote\"backslash\\newline\nand-$dollar".to_string()),
    );
    let mut outer = IndexMap::new();
    outer.insert("name".to_string(), Value::String("alpha".to_string()));
    outer.insert(
        "peers".to_string(),
        Value::List(vec![Value::String("a".into()), Value::String("b".into())]),
    );
    outer.insert("config".to_string(), Value::Map(inner));
    outer.insert("retired".to_string(), Value::Nil);
    outer.insert("ratio".to_string(), Value::Number(0.75));
    outer.insert("with-dash".to_string(), Value::Number(1.0));
    let original = Value::Map(outer);

    for formatted in [original.encode().unwrap(), original.encode_pretty().unwrap()] {
        let reparsed = parse(&formatted)
            .unwrap_or_else(|e| panic!("re-parse failed: {e}\nformatted: {formatted}"));
        assert_eq!(original, reparsed);
    }
    let compact = original.encode().unwrap();
    assert_eq!(parse(&compact).unwrap().encode().unwrap(), compact, "encode is idempotent");
    let pretty = original.encode_pretty().unwrap();
    assert_eq!(parse(&pretty).unwrap().encode_pretty().unwrap(), pretty);
}

#[test]
fn encode_quotes_strings_that_look_like_keywords() {
    let v = Value::String("true".to_string());
    let s = v.encode().unwrap();
    assert_eq!(s, "\"true\"");
    assert_eq!(parse(&format!("[{s}]")).unwrap(), Value::List(vec![v]));
}

#[test]
fn encode_escapes_leading_tilde_only_where_it_would_expand() {
    for s in ["~", "~/", "~/foo", "~/.config/mixos"] {
        let v = Value::String(s.to_string());
        let emitted = v.encode().unwrap();
        assert!(emitted.starts_with("\"\\~"), "{s:?} -> {emitted}");
        assert_eq!(parse(&format!("[{emitted}]")).unwrap(), Value::List(vec![v]));
    }
    for s in ["~example.net", "~bus", "~.", "a~b", "a~/b"] {
        let v = Value::String(s.to_string());
        let emitted = v.encode().unwrap();
        assert!(!emitted.starts_with("\"\\~"), "{s:?} escaped needlessly: {emitted}");
        assert_eq!(parse(&format!("[{emitted}]")).unwrap(), Value::List(vec![v]));
    }
}

#[test]
fn encode_escapes_special_characters() {
    let v = Value::String("a\"b\\c\nd\te\rf$g\x1bh\u{1}i".to_string());
    let s = v.encode().unwrap();
    assert_eq!(parse(&format!("[{s}]")).unwrap(), Value::List(vec![v]));
}

#[test]
fn round_trips_numbers_bit_exactly() {
    let cases: &[f64] = &[
        0.0,
        42.0,
        -42.0,
        1e15,
        9_007_199_254_740_992.0,
        1e20,
        -1e20,
        1e30,
        1.25,
        -0.5,
        f64::MAX,
        f64::MIN_POSITIVE,
    ];
    for &n in cases {
        let s = Value::Number(n).encode().unwrap();
        let parsed = parse(&format!("[{s}]")).unwrap();
        let Value::List(items) = &parsed else { panic!() };
        let Value::Number(back) = items[0] else { panic!("{:?}", items[0]) };
        assert_eq!(n.to_bits(), back.to_bits(), "{n} -> {s:?} -> {back}");
    }
}

#[test]
fn encode_refuses_non_finite_numbers_anywhere() {
    for n in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        let err = Value::Number(n).encode().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Encode);
        assert!(err.message().contains("non-finite"), "{err}");
    }
    let mut m = IndexMap::new();
    m.insert("ok".to_string(), Value::Number(1.0));
    m.insert("bad".to_string(), Value::Number(f64::NAN));
    assert_eq!(
        Value::List(vec![Value::Map(m)]).encode().unwrap_err().kind(),
        ErrorKind::Encode
    );
}

#[test]
fn parse_file_reports_a_missing_file() {
    let err = strict::parse_file(std::path::Path::new("/nonexistent/example.conf.mix")).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Io);
    assert!(err.to_string().contains("/nonexistent/example.conf.mix"), "{err}");
}
