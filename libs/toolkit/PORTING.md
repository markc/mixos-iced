# Port history

## T0: port of cosmix-iced-widgets 0.1.7 to iced 0.15

Source: cosmix `src/desktop/crates/cosmix-iced-widgets`, revision
`685622493a5203635661fb9adc1cea4a4fc4ac3b`, ported on the compd repository
(branch `t0-toolkit`, head `1d6d32d7`) against its vendored iced 0.15.0-dev
and wgpu 30, then brought here verbatim before the G0 changes below. At T0
the crate still depended on `cosmix-design` (tokens, font weights) and
`cosmix-assets` (installed font set, Material Symbols catalogue); the
cosmix pin was unchanged (`68562249` already contained both).

## Features and layering

The default library has no window shell or selected renderer. The `wgpu` and
`tiny-skia` arms enable geometry for the audio canvases; gallery arms enable
the vendored iced application shell with Wayland only. The wgpu arm uses
`wgpu-bare` and the workspace's GLES backend, matching compd's GPU stack.
The naga 27 termcolor workaround is dropped: this graph uses vendored naga 30.
The gallery brings iced's exact web-sys 0.3.85 requirement into the lock;
Cargo consequently aligns the wasm-bindgen family to that version.

Workspace widget renderer selection moves to its existing engine consumers
without changing their enabled features. Toolkit is layer 1, with no engine
crate dependencies, below cos-iced (3) and cos-scene-iced (5). Neither host
depends on toolkit in T0.

## API adaptation and behaviour

- iced removed `Widget::children`; mutable `diff` now initialises and
  reconciles TextField and context-menu child trees. Tests explicitly diff
  newly created trees, matching the runtime lifecycle.
- Shell uses iced's local message Bus and carries window, waker, IME and
  clipboard requests. TextField uses `shell.local` and merges all requests.
- TextInput accepts borrowed fragments; the wrapper passes owned strings
  to retain its existing self-contained value and undo reconstruction. It
  refreshes that controlled fragment after edits, so subsequent layout and
  events retain the current value before the application rebuilds the widget.
- Text drawing explicitly keeps no ellipsis and no pixel hinting, matching
  the source controls. The removed TextInput `icon` style field was unused
  by TextField, which does not offer an icon.
- TextInput state is now generic over the renderer, which must be `'static`;
  snapshots store editor byte positions rather than 0.14 grapheme indices.
  The narrow vendored state access patch is documented in iced's PATCHES.md.
- Clipboard reads are asynchronous in iced 0.15: paste requests a read,
  and the text change arrives on a later clipboard event. Undo groups are
  split at both request and delivery; paste remains one undoable edit.
- iced's text editor captures empty IME preedit notifications; the menu
  still forwards them to its child, but the event status is now Captured
  rather than Ignored. The ported test asserts that new status.
- iced 0.15 includes its own undo bindings. TextField intercepts them so
  its existing bounded/coalesced history remains authoritative, including
  the existing suppression during composition and window blur.
- Text-input tests now use the real cosmic-text editor; the 0.15 no-op
  editor no longer implements text entry. Feature-graph assertions inspect
  normal/build feature edges, excluding iced's self dev-dependency that
  selects upstream defaults for its own tests and benchmarks.

No new widget features are introduced.

The gallery's existing English strings are in `i18n/en/toolkit.ftl`; only its
title changes to the new crate name. Preview and menu-default colours move
to the token path without changing their values. Fluent is gallery-only.

## G0: made generic

The crate is written for any iced project (decision: `toolkit` is generic
and stealable). Changes versus T0:

- **`tokens.rs`:** the `cosmix-design` mapping (`Tokens::from_colours`,
  `from_dictionary`, `TokenError`, the linear-light `colour` conversion) is
  gone. `Tokens` is now `{ palette: Palette, metrics: Metrics }` with plain
  fields, `Palette::dark()` (the T0 preview values, unchanged) and
  `Palette::light()` (new), and `Metrics::DEFAULT` (spacing, radii, border
  widths, type scale, weights). `text_input`, `menu_style` and
  `audio_style` derive the same styles as before from the palette, with the
  radius from `metrics.radius.md` (6 px) and the audio radius from
  `metrics.radius.sm` (4 px, the old `radius.min(4.0)`); `menu_style` also
  sets `text_size` from `metrics.text.md` (14 px, the old default).
  `tooltip_style()` takes no border-width argument: the width comes from
  `metrics.border.width` (1 px, the old default design metric). A theme
  compiler, if an application has one, maps its output onto these fields
  outside this crate.
- **`fonts.rs`:** the `cosmix-assets` discovery (`register_installed`,
  `material_icon`) is replaced by `FontSet` (sans, mono, serif, display,
  emoji; bytes or path), `IconFont` (font plus a `.codepoints` table, with
  its parser) and `fonts::install(set, icon) -> &'static Fonts`, once per
  process. Registration keeps the T0 behaviour: the supplied bytes replace a
  preloaded face of the same family, and the generic sans-serif, serif and
  monospace families are bound to the supplied roles. Family names are read
  from the font bytes rather than a manifest. `default_ui_font`,
  `default_mono_font` and `font_for` keep their signatures (the last
  argument now means "prefer the installed role"); the Light-to-Normal
  weight fallback (`cosmix_design::family_font_weight`) is
  `fonts::effective_weight`. `material_icon(name)` is `fonts::icon(name)`,
  returning `Option` (no installed set is simply `None`).
- **Tests:** text is shaped with iced's embedded Fira Sans (the
  `iced_graphics/fira-sans` dev feature) instead of a font file outside the
  crate, so `cargo test -p toolkit` works wherever the crate is taken.
- **Gallery:** `examples/gallery.rs` is a plain iced winit program (Wayland
  and X11) with the default fonts and a dark/light toggle over
  `Tokens::dark()`/`Tokens::light()`; `examples/gallery_fonts.rs` runs the
  same program after `fonts::install` with paths from the command line. The
  shared program is `examples/gallery/app.rs`. The compositor-nested capture
  gate that ran the gallery inside the desktop at T0 lives with the desktop
  tests, not here.
- **Feature graph:** the gallery arms are no longer Wayland-only;
  `tests/feature_graph.rs` checks they are plain winit programs on both
  backends with none of iced's default, debug or hot extras.
- **`tests/generic.rs`:** the gate (forbidden names in the crate; no
  workspace crate outside `vendor/` in the normal-dependency closure).
- The manifest states its own version, edition and toolchain, and names no
  project crate.
