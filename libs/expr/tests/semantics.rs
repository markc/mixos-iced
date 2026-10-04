//! The binding language pinned: what compiles, what is refused, and what
//! each construct evaluates to.

use std::collections::BTreeSet;
use std::time::Duration;

use expr::{Error, ErrorKind, Limits, Value, compile};
use serde_json::json;

fn run(source: &str) -> Result<Value, Error> {
    expr::eval(source, &[], &Limits::default())
}

fn run_with(source: &str, globals: &[(&str, &Value)]) -> Result<Value, Error> {
    expr::eval(source, globals, &Limits::default())
}

fn ok(source: &str) -> Value {
    run(source).unwrap_or_else(|e| panic!("{source}: {e}"))
}

fn ok_with(source: &str, globals: &[(&str, &Value)]) -> Value {
    run_with(source, globals).unwrap_or_else(|e| panic!("{source}: {e}"))
}

fn err(source: &str) -> Error {
    match run(source) {
        Ok(v) => panic!("{source}: expected an error, got {v}"),
        Err(e) => e,
    }
}

fn err_with(source: &str, globals: &[(&str, &Value)]) -> Error {
    match run_with(source, globals) {
        Ok(v) => panic!("{source}: expected an error, got {v}"),
        Err(e) => e,
    }
}

fn model() -> Value {
    json!({
        "rows": [{"id": "a", "cells": ["one"]}, {"id": "b", "cells": ["two"]}],
        "prefix": "live ",
        "view": "list",
        "volume": {"has_icon": false},
        "title": "hello",
        "count": 3,
        "size": 13.0,
        "nothing": null,
        "svc": {"22": "ssh", "80": "http", "*": "unknown"},
        "k": "prefix",
    })
}

fn set(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|p| p.to_string()).collect()
}

#[test]
fn scene_bindings_evaluate_over_the_model() {
    let model = model();
    let item = json!({"id": "x", "cells": ["cell"], "hidden": true});
    let globals: &[(&str, &Value)] = &[("model", &model), ("item", &item)];
    assert_eq!(ok_with("$model.rows", globals), model["rows"]);
    assert_eq!(ok_with("$item.cells[0]", globals), json!("cell"));
    assert_eq!(ok_with("not $model.volume.has_icon", globals), json!(true));
    assert_eq!(ok_with("$model.view != \"x\"", globals), json!(true));
    assert_eq!(ok_with("$model.prefix .. $item.cells[0]", globals), json!("live cell"));
    assert_eq!(ok_with("$model.rows[0].id", globals), json!("a"));
    assert_eq!(ok_with("$model.rows[-1].cells[0]", globals), json!("two"));
    assert_eq!(ok_with("$item.hidden", globals), json!(true));
    assert_eq!(ok_with("$item.hide ?? false", globals), json!(false));
    assert_eq!(ok_with("$model.subtitle ?? \"n/a\"", globals), json!("n/a"));
    assert_eq!(ok_with("$model.subtitle ? 1 / 0 : \"n/a\"", globals), json!("n/a"));
    assert_eq!(ok_with("$model.fail ? 1 / 0 : $model.title", globals), json!("hello"));
    assert_eq!(ok_with("$model.size", globals), json!(13.0));
    assert_eq!(ok_with("$model.count * 2", globals), json!(6));
    assert_eq!(ok_with("$model[$model.k]", globals), json!("live "));
    let failed = err_with("$model.fail ? $model.title : 1 / 0", globals);
    assert_eq!(failed.kind, ErrorKind::Runtime);
    assert_eq!(failed.message, "division by zero");
}

#[test]
fn literals() {
    assert_eq!(ok("42"), json!(42));
    assert_eq!(ok("2.5"), json!(2.5));
    assert_eq!(ok(".5"), json!(0.5));
    assert_eq!(ok("1_000"), json!(1000));
    assert_eq!(ok("1e3"), json!(1000));
    assert_eq!(ok("2E-1"), json!(0.2));
    assert_eq!(ok("0xFF"), json!(255));
    assert_eq!(ok("0o755"), json!(493));
    assert_eq!(ok("0b101"), json!(5));
    assert_eq!(ok("0"), json!(0));
    assert_eq!(ok("0.5"), json!(0.5));
    assert_eq!(ok("true"), json!(true));
    assert_eq!(ok("false"), json!(false));
    assert_eq!(ok("nil"), Value::Null);
    assert_eq!(ok("'raw ${x} \\n'"), json!("raw ${x} \\n"));
    assert_eq!(ok("'it\\'s'"), json!("it's"));
    assert_eq!(ok("\"tab\\there\\n\""), json!("tab\there\n"));
    assert_eq!(ok("\"\\$5 \\\"q\\\" \\\\ \\~\""), json!("$5 \"q\" \\ ~"));
    assert_eq!(ok("\"\\u{2764} \\x41\\x42 \\d\""), json!("\u{2764} AB \\d"));
    assert_eq!(ok("\"cost: $name\""), json!("cost: $name"));
    assert_eq!(ok("[1, 'two', [3]]"), json!([1, "two", [3]]));
    assert_eq!(ok("[1, 2,]"), json!([1, 2]));
    assert_eq!(ok("{a: 1, \"b c\": 2, label: 3}"), json!({"a": 1, "b c": 2, "label": 3}));
    assert_eq!(ok("{\n  a: 1,\n  b: 2\n}"), json!({"a": 1, "b": 2}));
    assert_eq!(ok("normal"), json!("normal"));
    assert_eq!(ok("1 -- a comment"), json!(1));
    assert_eq!(ok("1 # a comment"), json!(1));
}

#[test]
fn number_literals_that_lose_value_are_refused() {
    for source in ["0755", "007", "9007199254740993", "1e999", "0x1G", "0o9", "0x", "0x20000000000001"] {
        let e = err(source);
        assert_eq!(e.kind, ErrorKind::Syntax, "{source}: {e}");
    }
    assert_eq!(ok("9007199254740992"), json!(9007199254740992i64));
}

#[test]
fn arithmetic_follows_mix() {
    assert_eq!(ok("7 + 3"), json!(10));
    assert_eq!(ok("7 - 3"), json!(4));
    assert_eq!(ok("7 * 3"), json!(21));
    assert_eq!(ok("7 / 3"), json!(7.0 / 3.0));
    assert_eq!(ok("7 % 3"), json!(1));
    assert_eq!(ok("-7 % 3"), json!(-1));
    assert_eq!(ok("7.5 % 2"), json!(1.5));
    assert_eq!(ok("2 ** 10"), json!(1024));
    assert_eq!(ok("2 ** 3 ** 2"), json!(64));
    assert_eq!(ok("-2 ** 2"), json!(4));
    assert_eq!(ok("9 ** 0.5"), json!(3));
    assert_eq!(ok("2 + 3 * 4"), json!(14));
    assert_eq!(ok("(2 + 3) * 4"), json!(20));
    assert_eq!(ok("true + true"), json!(2));
    assert_eq!(ok("\"5\" + 3"), json!(8));
    assert_eq!(ok("\"5\" + \"10\""), json!(15));
    assert_eq!(ok("\"8\" / \"2\""), json!(4));
    assert_eq!(ok("\"a\" + \"b\""), json!("ab"));
    assert_eq!(ok("\"inf\" + 1"), json!("inf1"));
    assert_eq!(ok("- \"3\""), json!(-3));
    assert_eq!(ok("4.0 / 2.0"), json!(2));
    assert_eq!(err("5 / 0").message, "division by zero");
    assert_eq!(err("5 % 0").message, "modulo by zero");
    assert_eq!(err("\"ab\" * 3").message, "cannot use 'ab' as number");
    assert_eq!(err("1 / \"x\"").message, "cannot use 'x' as number");
    assert_eq!(err("-\"x\"").message, "cannot negate string");
    assert_eq!(err("-nil").message, "cannot negate nil");
    let nil_add = err("nil + 1");
    assert_eq!(nil_add.kind, ErrorKind::Runtime);
    assert!(nil_add.message.starts_with("`+` is not defined for nil and number"), "{nil_add}");
    assert!(err("[1] + 2").message.starts_with("`+` is not defined for list and number"));
    assert!(err("{a: 1} + {b: 2}").message.starts_with("`+` is not defined for map and map"));
    assert_eq!(err("1e308 * 10").kind, ErrorKind::Runtime);
}

#[test]
fn concatenation_stringifies_both_sides() {
    assert_eq!(ok("\"x\" .. 1 .. \"y\""), json!("x1y"));
    assert_eq!(ok("1 .. 2"), json!("12"));
    assert_eq!(ok("\"sum=\" .. 2 + 3"), json!("sum=5"));
    assert_eq!(ok("true .. nil"), json!("truenil"));
    assert_eq!(ok("'' .. 2.5 .. ' ' .. 1e20 .. ' ' .. 1e-10"), json!("2.5 100000000000000000000 0.0000000001"));
    assert_eq!(ok("'' .. [1, 'a', nil]"), json!("[1, a, nil]"));
    assert_eq!(ok("'' .. {a: 1, b: [2]}"), json!("{a: 1, b: [2]}"));
    assert_eq!(ok("'a' ..\n  'b'"), json!("ab"));
    assert_eq!(err("'a' ..").kind, ErrorKind::Syntax);
}

#[test]
fn equality_and_ordering() {
    assert_eq!(ok("2 == 2.0"), json!(true));
    assert_eq!(ok("5 == \"5\""), json!(true));
    assert_eq!(ok("5 == \"5.0\""), json!(true));
    assert_eq!(ok("1 == true"), json!(false));
    assert_eq!(ok("nil == nil"), json!(true));
    assert_eq!(ok("3 != 4"), json!(true));
    assert_eq!(ok("[1, 2] == nil"), json!(false));
    assert_eq!(ok("[1, 2] == \"text\""), json!(false));
    assert_eq!(ok("1 < 2 == true"), json!(true));
    assert_eq!(ok("1 + 2 == 3"), json!(true));
    let both = err("[1, 2] == [1, 2]");
    assert_eq!(both.kind, ErrorKind::Runtime);
    assert!(both.message.contains("`==` is not defined for list and list"), "{both}");
    assert!(err("{a: 1} != [1]").message.contains("`!=` is not defined for map and list"));
    assert_eq!(ok("5 < 10"), json!(true));
    assert_eq!(ok("\"5\" < \"10\""), json!(true));
    assert_eq!(ok("\"apple\" < \"banana\""), json!(true));
    assert_eq!(ok("\"Zebra\" < \"apple\""), json!(true));
    assert_eq!(ok("\"abc\" <= \"abc\""), json!(true));
    assert_eq!(ok("\"5\" < \"abc\""), json!(true));
    assert_eq!(ok("5 >= 5"), json!(true));
    assert_eq!(ok("true > false"), json!(true));
    assert_eq!(err("5 < \"abc\"").message, "cannot compare 'abc' as number");
    assert_eq!(err("nil < 1").message, "cannot compare 'nil' as number");
    assert_eq!(ok("5 eq \"5\""), json!(true));
    assert_eq!(ok("\"5\" eq 5"), json!(true));
    assert_eq!(ok("5 eq \"5.0\""), json!(false));
    assert_eq!(ok("\"a\" ne \"b\""), json!(true));
}

#[test]
fn boolean_operators_return_the_deciding_operand() {
    assert_eq!(ok("0 or \"fallback\""), json!("fallback"));
    assert_eq!(ok("\"first\" or \"second\""), json!("first"));
    assert_eq!(ok("1 and \"kept\""), json!("kept"));
    assert_eq!(ok("\"\" and \"skipped\""), json!(""));
    assert_eq!(ok("true or false and false"), json!(true));
    assert_eq!(ok("true or 1 / 0"), json!(true));
    assert_eq!(ok("false and 1 / 0"), json!(false));
    assert_eq!(err("false or 1 / 0").message, "division by zero");
    assert_eq!(err("true and 1 / 0").message, "division by zero");
    assert_eq!(ok("not true"), json!(false));
    assert_eq!(ok("not 0"), json!(true));
    assert_eq!(ok("not \"\""), json!(true));
    assert_eq!(ok("not \"0\""), json!(true));
    assert_eq!(ok("not \"x\""), json!(false));
    assert_eq!(ok("not nil"), json!(true));
    assert_eq!(ok("not []"), json!(true));
    assert_eq!(ok("not [1]"), json!(false));
    assert_eq!(ok("not {}"), json!(true));
    assert_eq!(ok("!true"), json!(false));
    assert_eq!(ok("!!\"x\""), json!(true));
}

#[test]
fn nil_coalesce_keeps_falsy_non_nil() {
    let nil = Value::Null;
    assert_eq!(ok_with("$x ?? \"default\"", &[("x", &nil)]), json!("default"));
    assert_eq!(ok("0 ?? \"default\""), json!(0));
    assert_eq!(ok("\"\" ?? \"default\""), json!(""));
    assert_eq!(ok("false ?? \"x\""), json!(false));
    assert_eq!(ok_with("\"x = \" .. $v ?? \"d\"", &[("v", &nil)]), json!("x = nil"));
    assert_eq!(ok_with("\"x = \" .. ($v ?? \"d\")", &[("v", &nil)]), json!("x = d"));
    assert_eq!(ok_with("$a ?? 0 < 5", &[("a", &nil)]), json!(true));
    assert_eq!(ok("1 ?? 1 / 0"), json!(1));
    assert_eq!(err("nil ?? 1 / 0").message, "division by zero");
}

#[test]
fn ternary_is_right_associative_and_short_circuits() {
    let two = json!(2);
    assert_eq!(ok_with("$n > 0 ? \"pos\" : \"neg\"", &[("n", &two)]), json!("pos"));
    assert_eq!(
        ok_with("$n == 1 ? \"one\" : $n == 2 ? \"two\" : \"many\"", &[("n", &two)]),
        json!("two")
    );
    assert_eq!(ok("true ? 1 : 1 / 0"), json!(1));
    assert_eq!(ok("false ? 1 / 0 : 2"), json!(2));
    assert_eq!(ok("\"\" ? 1 : 2"), json!(2));
    assert_eq!(ok("true ? false : \"x\""), json!(false));
}

#[test]
fn if_expression() {
    let five = json!(5);
    let code = json!(404);
    assert_eq!(ok_with("(if $n > 2 then \"big\" else \"small\" end)", &[("n", &five)]), json!("big"));
    assert_eq!(ok_with("if $n > 2 then \"big\" else \"small\" end", &[("n", &five)]), json!("big"));
    assert_eq!(
        ok_with(
            "if $code == 200 then \"ok\" else if $code == 404 then \"not found\" else \"?\" end",
            &[("code", &code)]
        ),
        json!("not found")
    );
    assert_eq!(
        ok_with("if $code == 200 then \"ok\" elif $code == 404 then \"nf\" end", &[("code", &code)]),
        json!("nf")
    );
    assert_eq!(ok("\"status: \" .. if true then \"error\" else \"fine\" end"), json!("status: error"));
    assert_eq!(ok("if true then 1 else 2 end + 3"), json!(4));
    assert_eq!(ok("if false then 1 end"), Value::Null);
    assert_eq!(ok("if true then else 2 end"), Value::Null);
    assert_eq!(ok("if false then 1 else end"), Value::Null);
    assert_eq!(ok("(if true then\n  1\nelse\n  2\nend)"), json!(1));
    assert_eq!(ok("if true then 1 ; else 2 end"), json!(1));
    assert_eq!(ok("if true then 1 else 1 / 0 end"), json!(1));
    assert_eq!(err("if true then 1").kind, ErrorKind::Syntax);
    assert_eq!(err("(if true then 1 ) end").kind, ErrorKind::Syntax);
}

#[test]
fn field_access_and_indexing() {
    let m = json!({"name": "ada", "age": 36, "to": 1, "end": 2, "function": 3, "fn": 4});
    let l = json!([10, 20, 30]);
    let svc = json!({"22": "ssh", "80": "http", "*": "unknown"});
    let s = json!("héllo");
    let n = json!(5);
    let globals: &[(&str, &Value)] = &[("m", &m), ("l", &l), ("svc", &svc), ("s", &s), ("n", &n)];
    assert_eq!(ok_with("$m.name", globals), json!("ada"));
    assert_eq!(ok_with("$m[\"age\"]", globals), json!(36));
    assert_eq!(ok_with("$m.missing", globals), Value::Null);
    assert_eq!(ok_with("$m[\"missing\"]", globals), Value::Null);
    assert_eq!(ok_with("$m.to + $m.end", globals), json!(3));
    assert_eq!(ok_with("$m.function", globals), json!(3));
    assert_eq!(ok_with("$m.\"name\"", globals), json!("ada"));
    assert_eq!(ok_with("$l[0]", globals), json!(10));
    assert_eq!(ok_with("$l[2]", globals), json!(30));
    assert_eq!(ok_with("$l[-1]", globals), json!(30));
    assert_eq!(ok_with("$l[-3]", globals), json!(10));
    assert_eq!(ok_with("$l[-4]", globals), Value::Null);
    assert_eq!(ok_with("$l[5]", globals), Value::Null);
    assert_eq!(ok_with("$l[1.9]", globals), json!(20));
    assert_eq!(ok_with("$svc[80]", globals), json!("http"));
    assert_eq!(ok_with("$svc[12345]", globals), json!("unknown"));
    assert_eq!(ok_with("$svc.nope", globals), json!("unknown"));
    assert_eq!(ok_with("$s[1]", globals), json!("é"));
    assert_eq!(ok_with("$s[-1]", globals), json!("o"));
    assert_eq!(ok_with("$s[9]", globals), Value::Null);
    assert_eq!(ok_with("[10, 20, 30][1]", globals), json!(20));
    assert_eq!(ok_with("{a: {b: 1}}.a.b", globals), json!(1));
    assert_eq!(err_with("$n.x", globals).message, "cannot access field 'x' on number");
    assert_eq!(err_with("$m.missing.x", globals).message, "cannot access field 'x' on nil");
    assert_eq!(err_with("$m.missing[0]", globals).message, "cannot index nil with number");
    assert_eq!(err_with("$l[\"a\"]", globals).message, "cannot index list with string");
    assert_eq!(err_with("$n[0]", globals).message, "cannot index number with number");
}

#[test]
fn globals_and_undefined_variables() {
    let v = json!(7);
    assert_eq!(ok_with("$v", &[("v", &v)]), json!(7));
    let e = err("$foo.bar");
    assert_eq!(e.kind, ErrorKind::Runtime);
    assert_eq!(e.message, "undefined variable '$foo'");
    assert_eq!(ok("$1"), Value::Null);
    assert_eq!(err("$missing").message, "undefined variable '$missing'");
}

#[test]
fn interpolation() {
    let model = json!({"user": {"name": "ada", "n": 5, "f": 2.5, "none": null}, "list": [1, [2, 3]], "m": {"k": [0, {"x": "deep"}]}, "e": ""});
    let fallback = json!("fb");
    let nil = Value::Null;
    let globals: &[(&str, &Value)] = &[("model", &model), ("fallback", &fallback), ("q", &nil)];
    assert_eq!(ok_with("\"Hi ${model.user.name}\"", globals), json!("Hi ada"));
    assert_eq!(ok_with("\"n=${model.user.n} f=${model.user.f}\"", globals), json!("n=5 f=2.5"));
    assert_eq!(ok_with("\"${model.user.none}\"", globals), json!("nil"));
    assert_eq!(ok_with("\"${model.user.missing}\"", globals), json!("nil"));
    assert_eq!(ok_with("\"${model.user.name.deeper}\"", globals), json!("nil"));
    assert_eq!(ok_with("\"${model.list[1][0]} ${model.m.k[1].x}\"", globals), json!("2 deep"));
    assert_eq!(ok_with("\"${model.list[-1]}\"", globals), json!("[2, 3]"));
    assert_eq!(ok_with("\"${model.m[\"k\"][0]}\"", globals), json!("0"));
    assert_eq!(ok_with("\"[${NOPE ?? \"none\"}]\"", globals), json!("[none]"));
    assert_eq!(ok_with("\"[${NOPE ??}]\"", globals), json!("[]"));
    assert_eq!(ok_with("\"${model.missing ?? $fallback}\"", globals), json!("fb"));
    assert_eq!(ok_with("\"${q ?? 'anon'}\"", globals), json!("anon"));
    assert_eq!(ok_with("\"[${model.e ?? \"x\"}]\"", globals), json!("[]"));
    assert_eq!(ok_with("\"[${model.e ?: \"x\"}]\"", globals), json!("[x]"));
    assert_eq!(ok_with("\"[${model.user.n ?: \"x\"}]\"", globals), json!("[5]"));
    assert_eq!(ok_with("\"${model.missing ?? 1 + 1}\"", globals), json!("2"));
    assert_eq!(ok_with("\"${model.missing ?? \"lit\" .. $fallback}\"", globals), json!("litfb"));
    // A default cannot contain a `}`: the interpolation ends at the first one.
    assert_eq!(compile("\"${model.missing ?? \"${fallback}!\"}\"").unwrap_err().kind, ErrorKind::Syntax);
    let e = err_with("\"[${NOPE_NOT_SET}]\"", globals);
    assert_eq!(e.kind, ErrorKind::Runtime);
    assert!(e.message.contains("undefined variable '$NOPE_NOT_SET' in interpolation"), "{e}");
    assert_eq!(
        err_with("\"${model.user[0]}\"", globals).message,
        "cannot index map with number in interpolation"
    );
    assert_eq!(
        err_with("\"${model.user.n[0]}\"", globals).message,
        "cannot index number with number in interpolation"
    );
}

#[test]
fn denied_constructs_are_compile_errors() {
    let cases = [
        ("function ($x) = 1", "function literal"),
        ("fn ($x) = 1", "function literal"),
        ("false ? 1 : function ($x) = $x", "function literal"),
        ("sleep(1)", "sleep()"),
        ("time()", "time()"),
        ("abs(1)", "abs()"),
        ("time () .. ''", "time()"),
        ("length([1, 2])", "length()"),
        ("$(id)", "command substitution"),
        ("false ? 1 : $(echo hi)", "command substitution"),
        ("$model.f()", "method call .f()"),
        ("$model.x.unknown_method()", "method call .unknown_method()"),
        ("'X-' .. $s.upper()", "method call .upper()"),
        ("$f(1)", "call on a function value"),
        ("[1][0](2)", "call on a function value"),
        ("sh 'id'", "sh"),
        ("false ? 1 : sh \"id\"", "sh"),
        ("send 'x' y", "send"),
        ("print('x')", "print statement"),
        ("eprint('x')", "print statement"),
        ("$model.a = 1", "assignment"),
        ("$model.a.b = 1", "assignment"),
        ("$model[0] = 1", "assignment"),
        ("$x = 1", "assignment"),
        ("1 && 2", "statement chaining"),
        ("true || false", "statement chaining"),
        ("1 | cat", "pipe"),
        ("\"~/root\"", "environment-variable interpolation"),
        ("\"~\"", "environment-variable interpolation"),
        ("\"${q ?? sleep(1)}\"", "sleep()"),
        ("\"${q ?? $(echo hi)}\"", "command substitution"),
        ("\"${q ?? sh 'id'}\"", "sh"),
        ("\"${q ?? send 'comp' 'window.focus'}\"", "send"),
        ("\"${f()}\"", "function call in an interpolation"),
        ("\"${model.items.len()}\"", "function call in an interpolation"),
        ("for $i = 1 to 9", "for loop"),
        ("while true", "while loop"),
        ("loop", "loop statement"),
        ("export PATH = \"/tmp\"", "export statement"),
        ("select 1", "select statement"),
        ("address \"sh\"", "address block"),
        ("emit 'x' y", "emit statement"),
        ("on verb", "on handler registration"),
        ("source x.mix", "source statement"),
        ("include x.mix", "include statement"),
        ("return 1", "return statement"),
        ("try", "try statement"),
        ("die 'x'", "die statement"),
        ("alias a = b", "alias statement"),
        ("(if $x then for $i = 1 to 9\n$i\nend else 0 end)", "for loop"),
        ("(if $x then while false\n1\nend else 0 end)", "while loop"),
        ("(if $x then export PATH = \"/tmp/x\" else 0 end)", "export statement"),
        ("(if $x then print(\"spam\") else 0 end)", "print statement"),
        ("(if $x then sleep(1) else 0 end)", "sleep()"),
        ("(if false then 1 else sh \"id\" end)", "sh"),
        ("(if true then $model.a = $model.b; $model.a else $model.c end)", "assignment"),
        ("(if true then 1\n2 end)", "more than one statement"),
    ];
    for (source, construct) in cases {
        let e = match compile(source) {
            Ok(_) => panic!("{source}: must be refused"),
            Err(e) => e,
        };
        assert_eq!(e.kind, ErrorKind::NotAllowed, "{source}: {e}");
        assert!(e.message.contains(construct), "{source}: {e}");
        assert!(e.message.ends_with("is not allowed in an expression"), "{source}: {e}");
    }
}

#[test]
fn syntax_errors() {
    let cases = [
        "",
        "   ",
        "1; 2",
        "1\n2",
        "$model.x\n$model.y",
        "(",
        "($model.x",
        "[1, 2",
        "{a: 1",
        "{a: 1, a: 2}",
        "{a: 1,}",
        "'unterminated",
        "\"unterminated",
        "\"${unterminated\"",
        "\"${}\"",
        "\"${a b}\"",
        "\"${a[0}\"",
        "1 +",
        "+ 1",
        "1 2",
        "$",
        "a & b",
        "<<EOF\nx\nEOF",
        "if true then 1 else",
        "1 ? 2",
        "~",
        "do",
        "then",
        "@",
        "\"\\u{D800}\"",
        "\"\\u{}\"",
    ];
    for source in cases {
        let e = match compile(source) {
            Ok(_) => panic!("{source:?}: must not parse"),
            Err(e) => e,
        };
        assert_eq!(e.kind, ErrorKind::Syntax, "{source:?}: {e}");
    }
}

#[test]
fn nesting_is_bounded() {
    let parens = format!("{}1{}", "(".repeat(260), ")".repeat(260));
    let e = compile(&parens).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Syntax);
    assert!(e.message.contains("nesting too deep"), "{e}");
    let ternaries = format!("{}1", "true ? 1 : ".repeat(260));
    assert_eq!(compile(&ternaries).unwrap_err().kind, ErrorKind::Syntax);
    let chain = std::iter::repeat_n("'a'", 300).collect::<Vec<_>>().join(" .. ");
    let e = compile(&chain).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Syntax);
    assert!(e.message.contains("nesting exceeds 256"), "{e}");
    let fine = std::iter::repeat_n("'a'", 200).collect::<Vec<_>>().join(" .. ");
    assert_eq!(ok(&fine), json!("a".repeat(200)));
    let negations = format!("{}true", "not ".repeat(150));
    assert_eq!(ok(&negations), json!(true));
    let lists = format!("{}1{}", "[".repeat(150), "]".repeat(150));
    assert!(compile(&lists).is_ok());
    let interp = format!("\"${{a{}0{}}}\"", "[$a".repeat(250), "]".repeat(250));
    let e = compile(&interp).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Syntax);
    assert!(e.message.contains("nesting too deep"), "{e}");
}

#[test]
fn reads_and_roots_are_syntactic() {
    let e = compile("$model.a.b .. $model.c").unwrap();
    assert_eq!(*e.reads(), set(&["model.a.b", "model.c"]));
    assert_eq!(*e.roots(), set(&["model"]));
    let e = compile("$model.m[$model.k].x").unwrap();
    assert_eq!(*e.reads(), set(&["model.m", "model.k"]));
    let e = compile("$model.prefix .. $item.cells[0]").unwrap();
    assert_eq!(*e.reads(), set(&["model.prefix", "item.cells"]));
    assert_eq!(*e.roots(), set(&["item", "model"]));
    assert_eq!(*compile("$foo.bar").unwrap().roots(), set(&["foo"]));
    assert_eq!(*compile("$foo[0]").unwrap().reads(), set(&["foo"]));
    assert_eq!(*compile("\"${HOME}\"").unwrap().roots(), set(&["HOME"]));
    assert_eq!(*compile("\"Hi ${model.user.name}\"").unwrap().reads(), set(&["model.user.name"]));
    assert_eq!(
        *compile("\"${model.missing ?? $model.fallback}\"").unwrap().reads(),
        set(&["model.missing", "model.fallback"])
    );
    assert_eq!(*compile("\"${model.x ?? $foo.bar}\"").unwrap().roots(), set(&["foo", "model"]));
    assert_eq!(*compile("\"${model.list[$model.i].x}\"").unwrap().reads(), set(&["model.list", "model.i"]));
    assert_eq!(*compile("$model.a ? $model.b : [$model.c, {k: $model.d}]").unwrap().reads(),
        set(&["model.a", "model.b", "model.c", "model.d"]));
    assert_eq!(*compile("if $model.a then $model.b elif $model.c then 1 else $model.d end").unwrap().reads(),
        set(&["model.a", "model.b", "model.c", "model.d"]));
    assert_eq!(*compile("($model.a ? $x : $y).z[0]").unwrap().reads(), set(&["model.a", "x", "y"]));
    assert!(compile("1 + 2").unwrap().reads().is_empty());
    assert_eq!(compile("$model.x").unwrap().source(), "$model.x");
}

#[test]
fn limits_are_budget_errors() {
    let pad = json!("x".repeat(24));
    let limits = Limits { max_string_len: Some(32), ..Default::default() };
    let e = compile("$pad .. $pad").unwrap().eval(&[("pad", &pad)], &limits).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Budget);
    assert_eq!(e.message, "string length 48 exceeds limit 32");
    let e = compile("\"${pad}${pad}\"").unwrap().eval(&[("pad", &pad)], &limits).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Budget);
    let e = compile("$pad + $pad").unwrap().eval(&[("pad", &pad)], &limits).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Budget);
    assert_eq!(compile("$pad").unwrap().eval(&[("pad", &pad)], &limits).unwrap(), pad);

    let limits = Limits { max_list_len: Some(2), ..Default::default() };
    let e = compile("[1, 2, 3]").unwrap().eval(&[], &limits).unwrap_err();
    assert_eq!(e.message, "list length 3 exceeds limit 2");
    let e = compile("[[1, 2, 3]]").unwrap().eval(&[], &limits).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Budget);
    assert_eq!(compile("[1, 2]").unwrap().eval(&[], &limits).unwrap(), json!([1, 2]));

    let limits = Limits { max_map_len: Some(1), ..Default::default() };
    let e = compile("{a: 1, b: 2}").unwrap().eval(&[], &limits).unwrap_err();
    assert_eq!(e.message, "map size 2 exceeds limit 1");

    let limits = Limits { time_limit: Some(Duration::ZERO), ..Default::default() };
    for source in ["42", "(if true then 42 else 0 end)", "'a' .. 'b'"] {
        let e = compile(source).unwrap().eval(&[], &limits).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Budget, "{source}");
        assert_eq!(e.message, "time limit exceeded", "{source}");
    }
    let limits = Limits { time_limit: Some(Duration::from_secs(60)), ..Default::default() };
    assert_eq!(compile("42").unwrap().eval(&[], &limits).unwrap(), json!(42));

    let limits = Limits { max_steps: Some(3), ..Default::default() };
    let e = compile("1 + 2 + 3").unwrap().eval(&[], &limits).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Budget);
    assert_eq!(e.message, "evaluation step limit 3 exceeded");
    assert_eq!(compile("1 + 2").unwrap().eval(&[], &limits).unwrap(), json!(3));
}

#[test]
fn results_are_plain_json() {
    assert_eq!(ok("40 + 2"), json!(42));
    assert_eq!(ok("7 / 2"), json!(3.5));
    assert_eq!(ok("-0.0"), json!(0));
    assert_eq!(ok("1e20"), json!(100000000000000000000.0));
    let rows = json!([{"id": "a"}]);
    let model = json!({"rows": rows.clone()});
    assert_eq!(ok_with("$model.rows", &[("model", &model)]), rows);
    let e = compile("$model.rows").unwrap();
    assert_eq!(e, e.clone());
    assert_eq!(format!("{}", ErrorKind::NotAllowed), "not allowed");
    assert_eq!(format!("{}", err("1 / 0")), "division by zero");
}
