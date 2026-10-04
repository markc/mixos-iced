# Changelog

## 0.2.0

- Theming: `theme::Theme`, a complete iced theme made of `Tokens`:
  `theme::Base` plus the `Catalog` of button, text_input, checkbox, radio,
  toggler, slider, pick_list and its menu, combo_box, scrollable,
  container, progress_bar, rule, pane_grid, text_editor, text, table,
  float and svg (feature `svg`), with named classes for the button, container
  and text variants. The name fingerprints the tokens, so a live swap
  restyles the next frame. `Theme::named`, `Theme::to_iced`.
- `theme::Catalog` (`menu_style`, `audio_style`) for `Theme` and
  `iced_core::Theme`; `Menu`, `Panel`, `Fader`, `Knob`, `LevelMeter`,
  `Toggle`, `Waveform` and `PianoRoll` take their colours from the theme
  unless styled explicitly. **Breaking:** those widgets' `Theme` parameter
  now needs `theme::Catalog`, and `TextField` has a `Theme` type parameter
  (`TextField<'a, Message, Theme, Renderer>`).
- Icons: `icon(name)` / `Icon`, a glyph from the installed `IconFont` as a
  text widget; `icons::freedesktop::{Lookup, Resolver}`, a std-only icon
  theme and desktop-entry resolver.
- Gates: `tests/colours.rs` (no colour literal outside `src/tokens.rs`,
  with a planted self-check); `tests/snapshots.rs` renders the gallery
  offscreen to PNG under every token set (feature `gallery-tiny-skia`).
- Gallery: Dark, Light and Custom token sets switchable at runtime, and a
  page of iced's built-in widgets under the theme.

## 0.1.0

First release, for iced 0.15.0-dev.

- Widgets: `TextField` (bounded, selection-aware undo/redo, secure mode,
  submit), `Menu`/`Item`/`Panel`/`Navigator` (menu bar and context menus
  with keyboard navigation; in-surface overlays or app-owned popups via
  `MenuState`), `Fader`, `Knob`, `LevelMeter`, `Toggle` (with the dB taper
  in `scale`), `Waveform`/`WaveformPeaks`, `PianoRoll`/`RollNotes`/`RollView`.
- Theming: `Tokens { palette: Palette, metrics: Metrics }` with built-in
  `dark()` and `light()` sets; `text_input`, `menu_style`, `tooltip_style`
  and `audio_style` derive every widget style from them.
- Fonts and icons: `FontSet` (sans, mono, serif, display, emoji; bytes or
  path), `IconFont` (font plus a `.codepoints` table) and `fonts::install`,
  once per process; `default_ui_font`, `default_mono_font`, `font_for`,
  `fonts::icon`.
- Features: `wgpu`, `tiny-skia`, and the `gallery-*` arms for the
  examples; the library links no window shell and selects no renderer.
- Examples: `gallery` (default fonts, dark/light toggle) and
  `gallery_fonts` (a `FontSet`/`IconFont` from command-line paths).
- Tests: widget unit tests over a measurement-only renderer,
  `tests/feature_graph.rs` and the `tests/generic.rs` gate.
