# toolkit

Event-driven widgets for [iced](https://iced.rs) 0.15 with plain-data
theming. It is written for any iced project: nothing in it names the
project it ships with, it depends on nothing but the iced crates, and a
gate (`tests/generic.rs`) keeps it that way.

## Widgets

| Widget | What |
|---|---|
| `TextField` | single-line input with bounded, selection-aware undo/redo, secure mode, submit |
| `Menu`, `Item`, `Panel`, `Navigator` | menu bar and context menus with keyboard navigation; in-surface overlays by default, or app-owned popups via `MenuState` |
| `Fader`, `Knob`, `LevelMeter`, `Toggle` | pro-audio strip controls (dB taper in `scale`, peak hold, mute/solo toggles) |
| `Waveform`, `WaveformPeaks` | peak-file waveform with playhead and seek |
| `PianoRoll`, `RollNotes`, `RollView` | tiled, cached piano roll for very large note sets |
| `Icon` (`icon(name)`) | a named glyph from the installed icon font, as text |

Everything is a plain `iced_core::Widget`. The library selects no renderer
and links no window shell; the host enables the `wgpu` or `tiny-skia`
feature.

## Theming: `Tokens` and `Theme`

```rust
use toolkit::{Palette, Metrics, Tokens, Theme, theme};

let tokens = Tokens::dark();            // or Tokens::light(), or your own:
let mine = Tokens::new(Palette { primary: my_colour, ..Palette::light() }, Metrics::DEFAULT);

// The whole look: use toolkit's Theme as the application's iced theme type.
iced::application(App::new, App::update, App::view)
    .theme(|app: &App| Theme::new(app.tokens))
    .run()?;

// Every iced built-in and every toolkit widget is then styled from the
// tokens by default; a class or closure overrides one instance.
button("Delete").style(theme::button::destructive);
container(card).style(theme::container::card);
text("hint").style(theme::text::muted);
Menu::bar(items);                       // theme.menu_style()
Fader::new(db);                         // theme.audio_style()
TextField::new("Name", &value);         // tokens.text_input(status)
```

- `Palette`: 19 iced `Color`s in surface/text pairs (`surface`/`text`,
  `popover`, `elevated`, `card`, `primary`, `destructive`, `muted_surface`/
  `muted_text`, `selection`) plus `border`, `input` and `ring`. `dark()` and
  `light()` are complete and contrast-checked by the tests.
- `Metrics`: `spacing` (xs–xl), `radius` (sm/md/lg), `border` (width,
  focus_width), `text` (xs–xxl sizes) and `weight` (light/regular/medium/
  bold). `Metrics::DEFAULT` is 2/4/8/16/24, 4/6/12, 1/2, 11–24, 300–700.
- `Tokens { palette, metrics }` derives the explicit widget styles:
  `text_input(status)`, `menu_style()`, `tooltip_style()`, `audio_style()`.
- `Theme` (`theme::Theme`) wraps a `Tokens` and implements iced's
  `theme::Base` and the `Catalog` of button, text_input, checkbox, radio,
  toggler, slider, pick_list and its menu, combo_box, scrollable, container,
  progress_bar, rule, pane_grid, text_editor, text, table, float and (with
  the `svg` feature) svg, plus `theme::Catalog` for this crate's widgets.
  The default classes are the toolkit styles; the named ones are
  `theme::button::{primary, secondary, destructive, text}`,
  `theme::container::{transparent, surface, card, popover, elevated,
  tooltip}`, `theme::text::{default, muted, primary, destructive}` and
  `theme::svg::symbolic`. Every style is a pure function of the tokens, so
  `Theme::set_tokens` (or returning a new `Theme` from the application's
  `theme` function) restyles everything on the next frame; the theme's name
  is a fingerprint of the tokens, which is how widgets that cache by theme
  name notice. `Theme::named(tokens, "Night")` labels one for a picker;
  `Theme::to_iced()` gives an `iced::Theme` for third-party widgets.
- `theme::Catalog` (`menu_style()`, `audio_style()`) is implemented for
  `Theme` and for `iced::Theme`, so the toolkit widgets also work under
  iced's own themes.

No theme file format: an application maps its own theme onto these fields.
`tests/colours.rs` fails the build if any colour is constructed from
numbers anywhere in `src/` but `tokens.rs` (the palettes): derived shades
are mixes of palette colours.

## Fonts and icons: `FontSet`, `IconFont`, `fonts::install`

```rust
use toolkit::{FontSet, IconFont, fonts};

let set = FontSet::new()
    .sans(std::path::Path::new("fonts/Sans.ttf"))     // a path, read at install
    .mono(include_bytes!("fonts/Mono.ttf").as_slice()) // or bytes
    .display(display_bytes_vec)
    .emoji(std::path::Path::new("fonts/Emoji.ttf"));
let icons = IconFont::from_codepoints(
    std::path::Path::new("icons/Symbols.ttf"),
    &std::fs::read_to_string("icons/Symbols.codepoints")?,   // "name hex" per line
)?;
let fonts = fonts::install(set, Some(icons))?;             // once per process

iced::application(...).default_font(fonts::default_ui_font());
text("Heading").font(fonts.font(Role::Display).unwrap());
let (glyph, font) = fonts::icon("delete").unwrap();
text(glyph.to_string()).font(font);
```

- `FontSource`: `Bytes(Cow<'static, [u8]>)` or `Path(PathBuf)`; `From` for
  `&'static [u8]`, `Vec<u8>`, `PathBuf` and `&Path`.
- `FontSet`: one optional `FontSource` per `Role` (`Sans`, `Mono`, `Serif`,
  `Display`, `Emoji`).
- `IconFont { font, codepoints }`: `parse_codepoints(text)` reads the
  Material Symbols `.codepoints` format and rejects bad rows with a line
  number.
- `fonts::install(set, icon) -> Result<&'static Fonts, FontError>` reads
  the sources, replaces any preloaded face of the same family, registers
  them with iced's font system and binds the generic sans-serif, serif and
  monospace families to the sans, serif and mono roles. It runs once per
  process; a second call is `FontError::AlreadyInstalled`.
- `Fonts`: `family(role)`, `font(role)`, `icon_font()`, `icon(name)`,
  `icon_names()`. Module-level `fonts::installed()`, `fonts::icon(name)`,
  `fonts::default_ui_font()`, `fonts::default_mono_font()` and
  `fonts::font_for(family, fallbacks, weight, monospace, prefer_installed)`
  work before and after `install` (before it, they resolve to iced's
  generic families).

The crate never reads a path, an environment variable or a manifest; the
caller decides where fonts come from.

### Icon widgets: `icon`

```rust
use toolkit::icon;

button(row![icon("delete").size(18), "Delete"]);   // glyph in the icon font
icon("warning").color(tokens.palette.destructive); // a fixed colour
```

`icon(name)` is a text widget showing the named glyph of the installed
`IconFont` in the surrounding text colour (so it follows a button's text
under every theme) at the text size unless `.size()` is given. A name the
table lacks renders as the name itself, so a missing icon is visible.

### Icon files on disk: `icons::freedesktop`

```rust
use toolkit::icons::freedesktop::{Lookup, Resolver};

let icons = Resolver::new(Lookup::from_xdg());          // or Lookup::in_data_dirs(theme, dirs)
let path = icons.resolve("firefox", 32, Some("org.mozilla.firefox.desktop"));
```

A std-only resolver for the freedesktop Icon Theme and Desktop Entry
specifications: `Lookup::find(name, size)` walks the selected theme, its
`Inherits` chain, the fallback themes (hicolor, Adwaita, AdwaitaLegacy) and
the pixmap directories, trying `-symbolic` and plain spellings;
`Lookup::resolve(source, size, app)` adds the desktop-entry route (the
entry's `Icon=`, or its category) and the placeholder icons. The caller
chooses the directories and theme (`Lookup::new`, `in_data_dirs`); only
`from_xdg` reads the XDG environment and the GTK/KDE settings files.
`Resolver` adds a bounded cache that re-checks the file on every hit.

## Strings

The widgets draw only what the application gives them. The gallery's own
labels come from `i18n/en/toolkit.ftl` (Fluent), used by the examples only.

## Examples

```sh
cargo run -p toolkit --example gallery --features gallery-wgpu        # default fonts
cargo run -p toolkit --example gallery_fonts --features gallery-wgpu -- \
    --sans Sans.ttf --mono Mono.ttf --icons Symbols.ttf               # a supplied FontSet
```

`gallery-tiny-skia` selects the software renderer instead. Both examples
are ordinary iced winit programs (Wayland and X11). The gallery's Dark,
Light and Custom buttons swap the tokens while it runs.

```sh
cargo test -p toolkit --features gallery-tiny-skia --test snapshots
```

renders the gallery page offscreen (iced's headless simulator, software
renderer, embedded Fira Sans) under each token set and writes
`target/tmp/toolkit-snapshots/gallery-{dark,light,custom}.png` plus a
before/after pair for the live swap, checking the clear colour, a
primary-filled control and that the sets differ. No window or GPU is
needed, so it runs on a build server.

## Taking it

- **Git dependency:** `toolkit = { git = "<this repository>", rev = "<rev>" }`.
  The crate resolves its iced crates through the workspace it lives in, so
  the pinned iced revision comes with it.
- **Vendoring:** copy `libs/toolkit/`, then point the `iced*` dependencies at
  your own iced 0.15.0-dev checkout (one path each; the crate uses
  `iced_core`, `iced_widget`, `iced_graphics`, `iced_renderer`,
  `iced_runtime`, optionally `iced_wgpu` and the `iced` umbrella for the
  examples). `repository.workspace = true` is the only other workspace
  reference.
- **One iced patch is required.** `TextField`'s selection-aware undo reads
  the text input's cursor and restores its value, which upstream iced
  0.15.0-dev keeps private. The checkout this crate ships with carries two
  narrow accessors (`vendor/iced/PATCHES.md`, "Toolkit text-input state
  access": `text::Input::cursor()`, a public `text_input::State` with
  `cursor()` and `overwrite()`), marked `// toolkit:` in the two files.
  Apply the same lines to your iced, or take every widget except
  `TextField`.
- **Single widgets:** each widget is one file (`src/<widget>.rs`) over
  `AudioStyle` or `MenuStyle` from `tokens.rs` and the two-method
  `theme::Catalog` trait from `theme.rs`; copy the file, the style type and
  the trait (or give the widget an explicit `.style(...)` and drop the
  bound). `icons/freedesktop.rs` stands alone.

Tests: `cargo test -p toolkit` (no window, no GPU, no host font: text is
shaped with iced's embedded Fira Sans). `tests/feature_graph.rs` checks the
feature arms against the lock; `tests/generic.rs` is the gate described
above; `tests/colours.rs` is the colour-literal gate; `tests/snapshots.rs`
the offscreen gallery (see Examples).

## Licence

MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`). Design and porting
notes are in `PORTING.md`; releases in `CHANGELOG.md`.
