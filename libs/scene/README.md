# scene

The Mix Scenes document core, with no renderer and no interpreter. A scene
is a `---` envelope of headers followed by one ```` ```mix ```` fence
holding a strict-data map of nodes:

````text
---
scene: 1
name: about-panel
citizen: about-citizen
model: {"title":"About"}
---
```mix
root: {widget: "column", gap: 8, children: ["title"]}
title: {widget: "text", text: "= $model.title", size: 18, bold: true}
```
````

The crate provides:

- `parse` to a `SceneDocument`, `lint` for every diagnostic (codes in
  `ALL_CODES`), `resolve` to a `ResolvedScene` with defaults filled and
  bindings evaluated, `diff` between two resolved trees, `describe` for a
  family's ports, and `to_source` to write a document back out;
- `bindings`: `= expression` ports compiled once (`compile`), re-evaluated
  on a model patch (`reevaluate`) and instantiated per list row
  (`template_instantiate_with`);
- `evaluator`: the `Evaluator` trait the bindings go through, implemented
  by `ExprEvaluator` over the `expr` crate and used by default;
- `fixtures`: the test documents, exported for hosts' tests.

The node fence is parsed by `strict` and never executed. A binding may read
`$model` anywhere and `$item` inside a list template; calls, assignments,
statements and shell or Bus forms are refused at compile time with
`binding-policy`.

Test: `cargo test -p scene`.
