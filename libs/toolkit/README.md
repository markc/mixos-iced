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

Everything is a plain `iced_core::Widget`. The library selects no renderer
and links no window shell; the host enables the `wgpu` or `tiny-skia`
feature.

## Theming: `Tokens`

```rust
use toolkit::{Palette, Metrics, Tokens};

let tokens = Tokens::dark();            // or Tokens::light(), or your own:
let mine = Tokens::new(Palette { primary: my_colour, ..Palette::light() }, Metrics::DEFAULT);

TextField::new("Name", &value).style(move |_, status| tokens.text_input(status));
Menu::bar(items).style(tokens.menu_style());
Fader::new(db).style(tokens.audio_style());
container(tip).style(move |_| tokens.tooltip_style());
```

- `Palette`: 19 iced `Color`s in surface/text pairs (`surface`/`text`,
  `popover`, `elevated`, `card`, `primary`, `destructive`, `muted_surface`/
  `muted_text`, `selection`) plus `border`, `input` and `ring`. `dark()` and
  `light()` are complete and contrast-checked by the tests.
- `Metrics`: `spacing` (xs–xl), `radius` (sm/md/lg), `border` (width,
  focus_width), `text` (xs–xxl sizes) and `weight` (light/regular/medium/
  bold). `Metrics::DEFAULT` is 2/4/8/16/24, 4/6/12, 1/2, 11–24, 300–700.
- `Tokens { palette, metrics }` derives every widget style:
  `text_input(status)`, `menu_style()`, `tooltip_style()`, `audio_style()`.

No theme file format: an application maps its own theme onto these fields.

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
are ordinary iced winit programs (Wayland and X11).

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
- **Single widgets:** each widget is one file (`src/<widget>.rs`) over
  `AudioStyle` or `MenuStyle` from `tokens.rs`; copy the file and the style
  type.

Tests: `cargo test -p toolkit` (no window, no GPU, no host font: text is
shaped with iced's embedded Fira Sans). `tests/feature_graph.rs` checks the
feature arms against the lock; `tests/generic.rs` is the gate described
above.

## Licence

MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`). Provenance is in
`NOTICE`; the port history is in `PORTING.md` and `CHANGELOG.md`.
