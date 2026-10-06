# appearance

The MixOS look for an iced application, in one call. `toolkit` is generic
(it takes plain `Tokens` and a `FontSet` and knows nothing about MixOS);
this crate is the MixOS side of that line. It compiles the MixOS design,
maps it onto toolkit's tokens, turns the pinned asset set into toolkit's
font sources and installs them. toolkit never depends on it
(`tests/layering.rs`).

## One call

```rust
let look = appearance::install(&appearance::Theme::load())?;   // once, at startup
if let Some(warning) = look.warning() {                       // no or partial asset set
    log::warn!("{warning}");
}

iced::application(...)
    .default_font(look.ui_font())                             // Inter Variable, Light (300)
    .theme(|app: &App| app.look.theme());                     // toolkit::Theme from the tokens
TextField::new("Name", &value);                               // styled by the theme
Menu::bar(items);                                             // likewise
button("Delete").style(toolkit::theme::button::destructive);  // a per-widget class
let (glyph, font) = look.icon("delete").unwrap();             // Material Symbols Rounded
toolkit::icon("delete").size(18);                             // the same glyph as a widget
text("Title").font(look.display_font());                      // Quicksand

look.retheme(&appearance::Theme::from_source("settings", &new_source)?);  // live change
```

With `toolkit::Theme` as the application's iced theme type (`look.theme()`,
rebuilt from `look.tokens` whenever iced asks), every iced built-in and
toolkit widget takes the MixOS look with no per-widget style, and a
`retheme` restyles the next frame. `look.tokens` is still there for the
explicit styles (`text_input(status)`, `menu_style()`, `audio_style()`).

- `Theme::load()` reads `theme.conf.mix` from the MixOS etc directory
  (`theme_path()`), or uses the embedded default design. `Theme::read(path)`
  reports why a file is unusable; `Theme::from_source(identity, text)` takes a
  full design document or the shared selection-only file (`scheme:` and
  `mode:` alone, resolved against the embedded design); `Theme::for_context`
  compiles the embedded design for any scheme, mode and contrast.
- `install(&theme)` resolves the fonts from the activated MixOS asset set
  (`assets::mixos::select`), calls `toolkit::fonts::install` once and returns
  the `Appearance`: `tokens`, `context` (scheme, mode, contrast), `fonts`,
  `origin`. `install_with(&theme, sources)` takes sources from another
  lookup. A second call is `FontError::AlreadyInstalled`.
- `Appearance::retheme(&theme)` recomputes the tokens and typography; the
  fonts stay installed.
- `Appearance::theme()` is `toolkit::Theme::new(tokens)`: the iced theme
  whose name fingerprints the tokens, so widgets that cache by theme name
  notice a `retheme`.
- `Appearance::ui_font()`, `display_font()`, `mono_font()` are the installed
  faces at the design's `ui`, `ui_display` and `mono` weights. The pinned
  fonts are variable, so Light (300) renders true; without a set the
  record's family chain goes through toolkit's resolver, where Light on a
  family with no light face becomes Normal.

## What it maps

`tokens(&theme)` (or any `Resolved` design) gives `toolkit::Tokens`:

| toolkit | design |
|---|---|
| `surface` / `text` | pair `base`, rendered surface / foreground |
| `popover` / `popover_text` | pair `popover` (the compiler's alias of `elevated`) |
| `elevated` / `elevated_text` | pair `elevated` |
| `card` / `card_text` | pair `card`, composited over its backdrop |
| `primary` / `primary_text` | pair `primary` |
| `destructive` / `destructive_text` | pair `destructive` |
| `muted_surface` / `muted_text` | pair `muted` |
| `selection` / `selection_text` | pair `accent` (the tinted control pair) |
| `border`, `input`, `ring` | non-text `border`, `input`, `ring` |
| `spacing.xs … xl` | `spacing` scale, steps 1, 2, 4, 7, 9 (2, 4, 8, 16, 24 px) |
| `radius.md` | metric `radius`; `sm` is 2/3 of it, `lg` twice it |
| `border.width` | metric `button.border_width` |
| `text.md`, `xs`, `sm` | role `ui`, role `small`, metric `type.compact` |
| `weight.light`, `regular` | role `ui` (300), the default `md` button label (400) |

No source in the design, so toolkit's defaults stay: `border.focus_width`,
`text.lg`/`xl`/`xxl` (scaled from `md` by the default ratios),
`weight.medium`/`bold`. The design's `secondary` pair has no toolkit slot.
A pair the design lacks falls back to `base`, a non-text colour to the base
foreground, never to a literal. Every shipped scheme and mode passes
toolkit's contrast bar (AA text pairs, 3:1 fills and ring) in
`src/tokens.rs`'s tests.

`fonts()` gives `FontSources { set, icons, origin }` from the set's roles:
`sans` (Inter Variable), `mono` (JetBrains Mono), `serif` (Noto Serif),
`display` (Quicksand), `emoji` (Noto Color Emoji) as path sources, and
`icons` (Material Symbols Rounded) with the set's `.codepoints` catalogue as
the `IconFont`. Without a set the sources are empty and `origin` says why
(`NoSet { roots }`, `Unusable(error)`); `warning()` is the line to log. The
set's italic roles (`serif_italic`, `mono_italic`) have no toolkit role and
are not registered.

## Testing

`cargo test -p appearance`: the mapping on every shipped scheme and mode,
the theme sources, the font sources against fixture sets in a temporary
directory (`tests/fonts.rs`) and the dependency direction
(`tests/layering.rs`, which runs `cargo metadata --locked`).
