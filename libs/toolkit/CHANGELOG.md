# Changelog

## Unreleased

- Small services and helpers, each absorbed generic (sources in `NOTICE`):
  `measure::Measure` (widget bounds by id, scroll-corrected),
  `timers::Timers<K>` (one-shot debounces on one wake-on-deadline thread),
  `ime::Composition<Id>` (IME ownership across focus changes),
  `images::Images` (eager RGBA decode, mtime/len-validated cache; behind
  the new `image` feature, which links iced's decoder),
  `elide::middle` + `elide::Label` (middle elision that keeps the
  extension, bounded measured search), `fit_text::FitText` (font size
  solved from the bounds), `focus::Source` + `focus::ring`
  (`:focus-visible` convention) and `tips::tip`/`tips::regions` (themed
  tooltips, including hit regions over custom-drawn content).
- Gallery: a "Text" page (elision, fit-to-bounds, tooltips);
  `tests/snapshots.rs` writes `text-{dark,light}.png`.
- `CONTRIBUTING.md`: the custom-widget and quad-styling templates adapted
  from iced's `custom_widget`/`custom_quad` examples.
- Keys: `keys::Chord` (parsed from and shown as `Ctrl+Shift+S`; from key
  events by the Latin layout position), `keys::Bindings<A>` (chord → action
  in table order, accelerator labels), `keys::route` (modal, text-field and
  Alt+mnemonic precedence) and the root widgets `KeyRouter`, `Keys`
  (lossless key, IME, mouse and redraw reporting), `Inert` and `FocusProbe`.
- Dialogs: `dialog::Dialog` (message, confirm, prompt, secret, choice,
  progress) as application state with `update(Event) -> Option<Outcome>`,
  `view` and `key`; `dialog::modal` and the `Modal` frame (scrim, focus
  trap, declared focus), `Indeterminate` bar, `ModalQueue`, `Strings`.
  Keyboard only: Tab, Shift+Tab, Enter, Escape, arrows.
- Toasts: `toast::Toaster` with ids, limits and deadlines (`push`, `sweep`,
  `next_deadline`, `update`), `toast::overlay` stacking the cards in a
  corner and publishing `Event::Expired` from redraws; `Severity` shared
  with dialogs.
- Gallery: a "Dialogs & toasts" page driven from buttons and keys;
  `tests/services.rs` snapshots it dark and light and drives it through
  the simulator by keyboard alone.
- Data widgets: `virtual_list::VirtualList` builds, lays out and draws
  only the rows in view (a 100,000-row list costs a screenful per frame,
  checked by a test), with its own scrollbar, keyboard navigation (arrows,
  Page Up/Down, Home/End, Space, Enter, Ctrl+A, Escape, a type-ahead hook
  and `on_key` for the rest), `Selection` (sorted ranges, cursor, anchor;
  `Mode::{None, Single, Multiple}` with Ctrl and Shift), activation by
  Enter or double-click, `on_context`, stable row keys, an optional header
  with the `Columns` helper, `reveal` and the `scroll_to_row` task.
  `tree::TreeView` over `tree::Nodes` (a keyed model with lazy children):
  expand and collapse by expander, double-click, Right and Left, with
  indentation guides. Styled through `virtual_list::Catalog` and
  `tree::Catalog`, implemented for `Theme` and iced's theme.
- Gallery: a "Lists & trees" section with a 100,000-row list and a lazy
  tree; `tests/snapshots.rs` writes `lists-{dark,light}.png`.

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
