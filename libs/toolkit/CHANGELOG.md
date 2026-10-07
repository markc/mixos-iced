# Changelog

## 0.2.4

- Prepared immutable typography defaults and shared text/input builders let
  hosts change fonts, logical text size and line height without startup captures.
- Checked registered-font selection reports declared, installed-role or explicit
  generic choices, and refuses unavailable chains instead of hiding failure.

## 0.2.3

- Honour Light (300) in variable fonts whose weight axis covers it, even when
  fontdb indexes their default face at 400. Retain regular fallback for static
  families without a light face.

## 0.2.2

- Add a renderer-independent compound-pane example and offscreen checks for its
  Unicode input and token sets. Use the same panes in production applications.
- Add a reusable drag grip, per-tab text roles and a theme/renderer-generic
  requester using the shared undo/IME text field and double-click activation.

- Share virtual file rows through a borrowed listing provider, optional native
  transfer bridge and caller-supplied decorations. Cache and draw only visible
  rows; validate the pressed path after asynchronous relists.

- `EditorPane` exposes a neutral borrowed document source, typed editing
  intents and viewport geometry. Shared drawing and input retain Unicode
  clusters, bounded long-line checkpoints, annotations, IME and scroll echoes.
  Primary-selection messages leave platform clipboard support to the host.
- `TerminalPane` composes clipped renderer surfaces, focus borders and pane
  wheel actions without terminal-engine or renderer dependencies.
- `GridGeometry` shares fractional-scale cell hit-testing and IME cursor
  placement, including empty and malformed geometry handling.

## 0.2.1

- `CenteredButton` and `centered`: intrinsic labels and groups centre inside
  fixed, Fill or minimum-sized targets without compressible spacers. Generic
  theme/renderer support, caller-owned dimensions/padding, ordinary iced button
  press/style/disabled behaviour and gallery coverage under every token set.

## Unreleased

- Native drag areas carry the host's press token in the same widget message
  as the payload. `Session::start_with_gesture` avoids asynchronous subscription
  ordering races while retaining backend window, seat and liveness checks.

- Font sets accept additional faces alongside the named roles, for italic,
  bold and fallback faces chosen by the caller.

- Caller-owned `date_picker` and `time_picker`: validated Gregorian dates,
  leap years and ranges; localised calendars and 12/24-hour time editing,
  optional seconds, stable field identities and a single Tab owner.
- `patterns`: info strips, breadcrumbs, path/search fields, settings rows,
  header bars and scrollable About cards. All appear in the gallery under
  default and custom tokens, including narrow-window checks.
- Services: two-stroke bindings with pending-prefix cancellation and expiry,
  semantic success/warning roles, persistent `Modal::host`, focus traversal,
  palette pointer/keyboard/backdrop interaction and requester completion keys.
- Data widgets: reusable `RowHeights` prefix index and `Columns` resize
  Preview/Commit/Cancel lifecycle; scrollable tabs and middle-click close.
- Icons: PNG/SVG fallback assets with symbolic tint, scale-aware bounded
  metadata cache and external SVG resources disabled (`image` feature).
- Native DnD: portable `dnd::native::Session` and MIME codecs with explicit
  target acknowledgement and source completion; failed/cancelled transfers
  cannot remove a Move source. Native window adapters remain host-owned.
- `TextField` owns its input adapter over public iced editor traits; it
  needs no upstream state accessors. Secure Unicode input, selection-aware
  undo and composition ownership are covered against pristine iced.
- Shell chrome preserves the body widget tree when bars or sidebars change.
  The gallery covers the absorbed widgets with dark, light and custom
  tokens, variable rows, resizing and composed interaction flows.
- Application chrome in `shell::{Shell, Toolbar, Tool, StatusBar, Field,
  Side}` and the `tool`, `field`, `place`, `places` helpers: optional menu
  bar, centred toolbar with pinned edge tools, independent draggable
  sidebars, content and status. Labels and messages are caller-supplied;
  layout reads the supplied tokens and colours resolve from the live
  theme. The ordinary iced `shell` example has Fluent strings and
  headless rendering and interaction tests under dark, light and custom
  tokens.
- The file requester and widget-level drag and drop (T5; sources in
  `NOTICE`): `requester::{Filesystem, StdFs, Requester}` — an Open/Save
  picker as application state over a filesystem trait (std by default,
  a fake in tests; no GTK/rfd/portals), with `~` and relative-path
  resolution, Tab completion, directories-first capped listings, a
  hidden toggle, recents, and a token-styled view with localisable
  strings. `dnd::{DragArea, DropArea, Layer, State, find_zones}` — a
  generic-payload drag gesture shared between source, targets and the
  window-wrapping layer that draws the preview and Move/Copy/Cancel
  choice card (dopus's drag layer) plus iced_drop's drop-zone
  operation.
- Colour picker from B0ney's iced-color-picker (MIT; source in `NOTICE`):
  `color_picker::{Hsv, Component, Spectrum, ColorPicker}` — HSV/RGBA
  conversions round-trip tested, saturation×value matrix and hue strips
  drawn through iced's geometry API (both renderers, no extra feature),
  picking on press, drag and touch with a separate right-click callback,
  cached geometry redrawn only when the spectrum's own components move,
  and the marker outline derived from the picked colour's achromatic
  pole. Gallery: a picker field, hue strip and hex swatch on the "More"
  page.
- Overlay widgets (sources in `NOTICE`): `anchor` — the flip-then-shift
  placement core (Side, Align, Placement, `place`, safe hover corridors,
  viewport Anchor) from A-Disruption, pure math with tests;
  `popover::Popover` — a trigger with an anchored surface using that
  placement, outside-click/Escape dismiss; `collapsible::Collapsible` —
  a focusable header button (▸/▾) that shows and hides a body;
  `spinners::{Circular, Linear}` — self-redrawing indeterminate progress
  from iced's loading_spinners example; and
  `command_palette::{Command, fuzzy_match, filter, command_palette}` —
  the Ctrl+Shift+P surface with Sublime-style fuzzy ranking from
  iced_palette.
- Table and split, each absorbed generic (sources in `NOTICE`):
  `table::Table` over the `table::Column` trait (header/cell/footer,
  width + live resize offset), header and body (and optional footer) as
  separately-id'd scrollables synced by one `on_sync` message,
  drag-to-resize dividers per column, zebra rows and a lazy band catalog
  (`Style` token resolved at draw; `()` default) for `Theme` and
  `iced_core::Theme`, plus `table::sort_label` (a header label with
  ↑/↓ on the active sort — the generic shape of dopus's column
  header). `split::Split`: two panes with a draggable grip, relative
  (`0.0..=1.0`) or absolute (start/end) positions, `on_drag`/`on_drag_start`/`on_drag_end`/`on_double_click`
  (double-click reset), pane minimums from the children's own `Length`.
- Inputs and tabs absorbed from iced_aw (MIT; sources in `NOTICE`):
  `typed_input::TypedInput` (a text input whose value is `T: FromStr`),
  `number_input::NumberInput` (bounds, step, ▲▼/+- modifiers, wheel and
  arrows, character-level validation), `tab_bar::TabBar` +
  `tab_bar::TabLabel` (icon/text labels, close glyphs, hover tracking),
  `tabs::Tabs` (bar + active content, top or bottom),
  `sidebar::Sidebar` (the vertical shape, rows sized by
  `flush_column::FlushColumn`) and `flush_column::FlushColumn` itself
  (rows of one width with a flushed edge). Catalogs for `Theme` and
  `iced_core::Theme`; the bake-off with ced's tab strip went to the
  iced_aw shape (ced's is an application view; it rebuilds on
  `TabBar` when ced moves to toolkit).
- Widgets absorbed from iced_aw (master `f80f659`, MIT; sources in
  `NOTICE`), each restyled from `Tokens` through its own `Catalog`
  (implemented for `Theme` and `iced_core::Theme`): `badge::Badge`
  (primary/neutral/destructive), `card::Card` (head/body/foot, close
  glyph), `labeled_frame::LabeledFrame` (title in the frame edge),
  `selection_list::SelectionList` (scrollable, hover/selection rows,
  virtual text operations), `slide_bar::SlideBar` (token-styled track and
  fill, stepped drags), `spinner::Spinner` (self-redrawing orbit),
  `drop_down::DropDown` (nine alignments, viewport-clamped, dismiss on
  outside click or Escape) and `wrap::Wrap` (horizontal and vertical
  flow). New `num-traits` dependency for `SlideBar`'s stepped values.
- Gallery: a "More widgets" page with all eight;
  `tests/snapshots.rs` writes `more-{dark,light}.png`.
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
