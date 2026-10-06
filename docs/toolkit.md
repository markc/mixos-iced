# Widget toolkit

`toolkit` is a reusable iced widget library. Applications supply their own
palette, metrics, fonts, icons, strings and state. It has no dependency on the
desktop, message bus or a particular asset set. `appearance` is the separate
adapter that maps this system's design files into toolkit tokens.

## Widgets and composition

The library includes menus, tabs and sidebars; validated text, number, date
and time inputs; colour and audio controls; tables, virtual lists and trees;
split panes, overlays, popovers and a command palette. Application patterns
include header bars, breadcrumbs, path bars, setting rows and About views.

`shell::Shell` composes an optional menu, toolbar, status bar and split
sidebars around application content. Keep its shape stable across updates
to retain child focus, selections and edit history. All widget colours and
sizes come through `Tokens`; a `Theme` can be replaced while running.

Virtual lists build only visible rows, including with caller-owned variable
row heights. Resizable columns report preview, commit and cancellation so
the application can keep header and body widths together.

`EditorPane`, `FilePane` and `TerminalPane` share larger controls through neutral
providers. The editor supplies selection, Unicode cell layout, annotations,
IME and bounded document seeks; its host applies editing intents and primary
selection messages. File rows and their header share responsive columns and
cached visible text, with stable paths checked after relists. The terminal
frames an injected drawing surface and supplies focus, scroll and cell geometry.
Applications supply document engines, filesystem work and PTYs.

The standalone `compounds` example shows all three with a string document,
a synthetic large listing and an injected terminal surface. The
[application host](application.md) supplies native startup and renderer
adapters for Ced, DOpus and Term.

## Services

Keyboard routing supports ordinary shortcuts and two-stroke sequences.
Persistent modal hosts preserve the underlying widget tree, trap focus and
suspend old input-method composition during handover. Dialogs and toasts are
caller-owned state machines. The application decides what an accepted
dialog or activated toast does.

The file requester uses a filesystem trait with a standard local adapter;
tests or applications can supply another implementation. It supports Open
and Save, completion, navigation, hidden files and recent locations.

Named icons can use a caller-supplied font or freedesktop lookup. The optional
`image` feature supplies bounded eager PNG/SVG decoding, symbolic tinting,
scale-sensitive caching and a visible fallback for missing or invalid assets.

## Drag and drop

Local drag areas and drop zones compose inside an iced widget tree.
`dnd::native::Session` is a backend-neutral state machine for cross-window
transfer. A host adapter supplies real gesture identities, MIME/action
negotiation, nonblocking transfer and protocol completion. The target
acknowledges application of the delivered payload. A source removes a Move
payload only after successful native completion; Copy and interrupted
transfers retain it.

The developer tool `native-gallery` supplies a Wayland adapter outside the
library. Its two windows contain the ordinary gallery and a native drag strip.
The desktop acceptance gate drives the existing primary-seat Bus input path
against an isolated compositor. It checks payload hashes, exactly-once
delivery/completion, separate processes, overlapping offers, scaling,
cancellation, rejection, closure and stale gesture identities.

## Build and test

The crate README documents public APIs and integration examples. Its
changelog records additions and compatibility changes. Use the pinned
toolchain and dependency lockfile for repository builds:

```
cargo test -p toolkit --locked
cargo test -p toolkit -p appearance --features toolkit/gallery-tiny-skia,toolkit/image,toolkit/svg --locked
cargo run -p toolkit --example gallery --features gallery-tiny-skia
```

The headless renderer suite captures every gallery page with Dark, Light and
Custom tokens, alongside narrow-layout and interaction tests. The pristine
iced gate copies the library into a separate workspace and builds against
unpatched upstream iced at its recorded base. Native desktop acceptance is
a separate gate and requires built compositor and gallery binaries.
