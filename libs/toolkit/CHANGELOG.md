# Changelog

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
