# expr

The pure expression language of Mix Scenes bindings. A scene binds a port to
an expression such as `= $model.rows`, `= not $model.volume.has_icon` or
`= $model.prefix .. $item.cells[0]`; this crate compiles the expression,
reports which globals it reads, and evaluates it over `serde_json::Value`
globals under size and time limits.

The language is the expression subset of Mix with every builtin denied:
literals, `$name` globals with field and index access, arithmetic,
`..` concatenation, comparisons, `and`/`or`/`??`, the ternary and the
`if … then … else … end` expression. Calls, assignments, statements and
shell or Bus forms are refused at compile time with `ErrorKind::NotAllowed`.
The crate does not depend on the Mix interpreter.

```rust
let model = serde_json::json!({"prefix": "live ", "rows": [{"cells": ["one"]}]});
let binding = expr::compile("$model.prefix .. $model.rows[0].cells[0]")?;
assert_eq!(binding.roots().iter().collect::<Vec<_>>(), ["model"]);
let text = binding.eval(&[("model", &model)], &expr::Limits::default())?;
assert_eq!(text, "live one");
```

Test: `cargo test -p expr`. The semantics are pinned in
`tests/semantics.rs`; the crate documentation in `src/lib.rs` lists what is
accepted, refused and how errors are classified.
