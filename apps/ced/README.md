# Ced

Wayland editor over the native `edit` Bus service. Ced uses local echo,
per-origin undo, live agent edits, Mix and scene highlighting, diagnostics,
recovery, and native clipboard and primary selection.

The GUI starts with local appearance while its existing Bus worker registers.
That worker prepares a validated appearance cache, retries an unavailable
broker through the shared supervisor, and installs authority changes before
acknowledging them. The status bar shows live appearance provenance and Bus
connection state through the shared Fluent formatter. GUI `app.describe` exposes
the same evidence and the current capture's cache persistence receipt.

Session writes start only after this instance owns its Bus name. An initial
name collision forwards launch paths asynchronously when the window has not
been used. A touched window or terminal refusal after registration preserves
the window, its appearance and deferred document requests. An initial terminal
refusal cannot open those requests until registration is explicitly recovered
or the application restarts; an unavailable broker recovers automatically
through the existing supervisor. Closing queues the
final session and drains settings and session writes within a shared two-second
budget on the existing worker. A timeout is reported without claiming a save.

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
