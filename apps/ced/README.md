# Ced

Wayland editor over the native `edit` Bus service. Ced uses local echo,
per-origin undo, live agent edits, Mix and scene highlighting, diagnostics,
recovery, and native clipboard and primary selection.

The shared buffer contract belongs to `libs/edit`. Ced owns its private
editor model and syntax adapter. The latter uses the pinned Microsoft Edit
compiler and runtime under `vendor/msedit`, with deterministic generated
definitions checked by tests.

Build and test on CBC at a pushed commit:

```
cargo test --locked -p edit -p editor-model -p syntax -p editd -p ced
cargo build --locked --profile release-fast -p editd -p ced
```

Run editd before Ced. Both discover the same noded through the shared
configuration rule and use ABP. The frontend also supports a headless mode
for the real Bus acceptance tests.
