# toolkit

Event-driven widgets for [iced](https://iced.rs) 0.15 with plain-data
theming. It is written for any iced project: nothing in it names the
project it ships with, it depends on nothing but the iced crates, and a
gate (`tests/generic.rs`) keeps it that way.

## Widgets

| Widget | What |
|---|---|
| `CenteredButton`, `centered(content)` | centred intrinsic labels or groups within caller-sized targets, including minimum-sized Shrink boxes |
| `TextField` | single-line input with bounded, selection-aware undo/redo, secure mode, submit |
| `Menu`, `Item`, `Panel`, `Navigator` | menu bar and context menus with keyboard navigation; in-surface overlays by default, or app-owned popups via `MenuState` |
| `Fader`, `Knob`, `LevelMeter`, `Toggle` | pro-audio strip controls (dB taper in `scale`, peak hold, mute/solo toggles) |
| `Waveform`, `WaveformPeaks` | peak-file waveform with playhead and seek |
| `PianoRoll`, `RollNotes`, `RollView` | tiled, cached piano roll for very large note sets |
| `Icon` (`icon(name)`) | a named glyph from the installed icon font, as text |
| `KeyRouter`, `Keys`, `Inert`, `FocusProbe` (`keys`) | chord routing at the root, lossless key capture, input blocking under a modal, focus clicks |
| `Dialog`, `Modal`, `ModalQueue` (`dialog`) | message, confirm, prompt, secret, choice and progress dialogs over a scrim, keyboard-operable |
| `Toaster`, `Toast` (`toast`) | stacked corner notices with severity, action, dismiss and expiry |
| `VirtualList`, `Selection`, `virtual_list::Columns` | a list that builds and draws only the rows in view (100,000 rows cost a screenful), keyboard navigation, single/multiple selection, activation, type-ahead, a column header, `scroll_to_row` |
| `TreeView`, `Nodes` | a tree over the virtual list: a keyed node model with lazy children, expand/collapse by expander, double-click, Right and Left, indentation guides |
| `shell::{Shell, Toolbar, StatusBar, places}` | optional menu bar, centred tools with pinned edge groups, independently resizable sidebars and a status strip, composed from the existing widgets |
| `DatePicker`, `TimePicker` (in their modules) | caller-owned Gregorian dates and clock times, validation, ranges, localisable calendar and 12/24-hour controls |
| `TypedInput`, `NumberInput`, `ColorPicker` | parsed values, bounded numeric steps and an HSV colour field |
| `TabBar`, `Tabs`, `Sidebar`, `FlushColumn` | scrollable tabs, middle-click close, selected page and aligned navigation |
| `Table`, `Split` | sortable and resizable columns, synchronised scrolling and draggable panes |
| `Badge`, `Card`, `LabeledFrame`, `SelectionList`, `SlideBar`, `Wrap`, `DropDown` | token-styled compositions, selections and flow layouts |
| `anchor`, `Popover`, `Collapsible`, `Spinner`, `spinners` | viewport placement, dismissable popovers, expansion and indeterminate progress |
| `command_palette` | fuzzy command search with keyboard, pointer and scroll handling |
| `patterns` | info strips, breadcrumbs, path/search fields, settings rows, header bar and About card |
| `requester` | Open/Save state and view over a caller-selected filesystem, completion, recents and overwrite outcome |
| `dnd`, `dnd::native` | in-window typed gestures and a backend-independent native source/offer session |
| `measure`, `timers`, `ime`, `elide`, `FitText`, `focus`, `tips`, `images` | bounds, deadlines, composition ownership, fitted/elided text, focus traversal, tips and optional decoded images |

Everything is a plain `iced_core::Widget`. The library selects no renderer
and links no window shell; the host enables the `wgpu` or `tiny-skia`
feature.

## Centred buttons and content

`CenteredButton` centres a label, icon or group inside the complete click
target. Set its dimensions and padding on the builder; the default fits its
content with no extra padding. It uses the host theme's normal button catalog
and retains iced's disabled, hover and press behaviour:

```rust,no_run
use toolkit::{CenteredButton, Theme, Tokens, widget::text};
use toolkit::core::{Element, Length};

let tokens = Tokens::dark();
let button: Element<'_, u8, Theme, toolkit::widget::Renderer> =
    CenteredButton::new(text("1").size(tokens.metrics.text.md))
        .width(Length::Shrink.min(tokens.metrics.text.md + 2.0 * tokens.metrics.spacing.md))
        .height(tokens.metrics.text.md + 2.0 * tokens.metrics.spacing.md)
        .padding(tokens.metrics.spacing.sm)
        .on_press(1)
        .into();
```

`centered(content)` provides the same placement as an ordinary iced container,
with the standard width/height/padding/style builders. It centres both axes;
`align_x` or `align_y` can override one axis. Padding defines the inner area,
so asymmetric padding intentionally shifts the content relative to the full
target. Minimum constraints are applied before centring; no Fill spacers are
inserted into a compressed row. Intrinsic groups retain their own gaps.

## Application shell

`shell::Shell` composes toolkit's `Theme` and the selected renderer. Its
menu is the existing `Menu`, its sidebars use `Split`, and the toolbar
and status are ordinary iced compositions. It holds no application state.
Labels, messages, optional icons and content all come from the caller:

```rust,no_run
use toolkit::{Tokens, widget::text};
use toolkit::shell::{self, Shell, Side, StatusBar, Toolbar};

#[derive(Clone)]
enum Message { Save, Select, Resize(Side, f32) }

let tokens = Tokens::light();
let view: toolkit::core::Element<'_, Message, toolkit::Theme, toolkit::widget::Renderer> =
    Shell::new(text("Document"))
        .tokens(tokens)
        .toolbar(Toolbar::new().push(shell::tool("Save", Message::Save)))
        .sidebar(Side::Left, 180.0, shell::places(tokens, vec![
            shell::place("Documents", true, Message::Select),
        ]))
        .on_split(Message::Resize)
        .status(StatusBar::new().left(vec![shell::field("Ready")]))
        .into();
```

Keep sidebar widths in the app and update the matching `Side` when its
resize message arrives. Pass the current tokens when rebuilding the view
so metrics and tooltips follow the app's theme; bar and tool colours
resolve from the live theme at draw time. Toolbar edge groups take equal
space, so the middle stays centred when their widths differ. Reduce the
tools for windows too narrow to contain them.

Run the complete sample with
`cargo run -p toolkit --example shell --features gallery-tiny-skia`
(or `gallery-wgpu`). `tests/shell.rs` exercises its menus, buttons,
navigation, resizing and theme changes through the headless simulator.

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

The caller supplies the font bytes or paths; the crate discovers no font
configuration or manifest.

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

With `image`, `icons::Assets` renders resolved PNG or SVG files as eager
RGBA handles, with optional symbolic tint and scale. Its bounded cache
keys include file metadata, size, tint and scale; external SVG resources
are disabled. Missing files fall back to the visible icon name. The host
supplies the resolver and owns the asset cache.

## Keys, dialogs and toasts

These are transport-free: the application owns the state, draws it, feeds
the events back and acts on the outcomes; nothing here knows about a bus
or a command.

```rust
use toolkit::keys::{self, Bindings, Routed};
use toolkit::dialog::{self, Dialog, Outcome};
use toolkit::toast::{self, Toast, Toaster};

// Chords: "Ctrl+Shift+S", "F3", "Alt+Up"; letters by Latin layout position.
let bindings = Bindings::from_table([("Ctrl+S", Action::Save), ("F3", Action::Next)])?;
let label = bindings.label(&Action::Save);              // Some("Ctrl+S") for a menu

// The window content, wrapped in order: toasts, then the modal, then the router.
let content = toast::overlay(content, &app.toaster, tokens, Message::Toast);
let content = dialog::modal_host(content, app.dialog.as_ref(), tokens, Message::Dialog);
keys::router(content, &bindings, Message::Route)
    .modal(app.dialog.is_some())          // a dialog owns the keyboard
    .text_field(app.find_bar_focused)     // it keeps Ctrl+C/V/Z and friends
    .mnemonics(['f', 'e', 'h'])        // Alt+E -> Routed::Menu(1)
    .on_unclaimed(|key, modifiers| ...)   // what no child took

// In update:
Message::Dialog(event) => if let Some(outcome) = app.dialog.as_mut()?.update(event) {
    app.dialog = None;                    // Cancelled, Accepted, Text(s), Chosen(i), Button(i)
    app.queue.next(&mut app.dialog, |_| true);
}
Message::Toast(event) => if let Some(id) = app.toaster.update(event) { /* the action */ }

app.queue.offer(&mut app.dialog, Dialog::confirm("Delete?", "This cannot be undone."));
let handle = app.toaster.push(Toast::new("Saved").severity(Severity::Success));
```

- `Dialog::{message, confirm, prompt, secret, choice, progress}` with
  `.severity`, `.buttons([Button::primary(..), Button::destructive(..),
  Button::cancel(..)])`, `.strings(&Strings { ok, cancel, close })`,
  `.value`, `.placeholder`, `.selected`, `.cancellable`, `.width`;
  `set_error` (disables the primary button), `set_progress`
  (`Progress::Indeterminate` or `Fraction`), `set_body`.
- Keyboard: Tab and Shift+Tab move between the field, the list and the
  buttons; Enter activates the focused control, or the default button from
  the field or list; Escape cancels; Up and Down move the choice; Left and
  Right move between buttons. The frame captures every key, withholds
  keyboard and IME input from the content under it, and focuses the
  prompt's field itself, so no focus task is needed.
- `Toaster::new().limit(5).default_timeout(Some(d))`; `push` returns the
  `ToastId` and deadline; `sweep(now)` for an application timer, or let
  `toast::overlay` request a redraw at the nearest deadline and publish
  `Event::Expired`. `Toast::new(title).body(..).severity(..).action(..)
  .sticky().dismissable(false)`.
- `Keys` is the lossless root capture for a terminal (`on_press`,
  `input_method`, `on_mouse`, `on_pointer`, `on_redraw`); `Inert` shows
  content but withholds keyboard and IME input; `FocusProbe` reports a
  click over a text field.
- Styles: `dialog::{card, scrim, focus_ring, button_style, option_style}`
  and `toast::style` are functions of the theme; `Severity::colour` is the
  text colour, semantic success/warning or destructive. Set semantic roles
  with `Theme::with_semantic`; the existing `Palette` shape stays compatible.
- `keys::SequenceBindings` parses two-stroke sequences such as
  `Ctrl+K Ctrl+C`. The router keeps a pending prefix per widget/window,
  expires it on a deadline, and clears it when a modal takes ownership.
- Keep `Modal::host` mounted while closed to preserve the base widget tree,
  focus, selection and undo. Closing the input method prevents stale queued
  composition events from reaching the restored field.
- `keys::KeyRouter::tab_navigation` supplies one root Tab owner and wraps traversal;
  `focus::cycle` is the explicit operation for a caller-owned focus task.
## Data widgets: `VirtualList` and `TreeView`

```rust
use toolkit::virtual_list::{Columns, Selection, VirtualList, scroll_to_row};
use toolkit::tree::{Children, Nodes, TreeView};

// Rows come from a closure, by index; only the rows in view are built.
let columns = Columns::new().column("Name", Fill).column("Size", 90.0);
VirtualList::new(items.len(), |i| columns.row([text(&items[i].name).into(), text(items[i].size()).into()]))
    .header(columns.header(Some((0, true)), Message::Sort))
    .selection(&self.selection)          // the app owns the Selection
    .on_select(Message::Select)          // click, Ctrl/Shift, arrows, Space, Ctrl+A, Escape
    .on_activate(Message::Open)          // Enter, double-click
    .type_ahead(|prefix, from| items[from..].iter().position(|it| it.name.starts_with(prefix)).map(|p| p + from))
    .id("files");
scroll_to_row("files", 4_000)            // a Task; or `.reveal(Some(row))`

// A tree: the app keeps the Nodes model and answers toggles.
let mut nodes = Nodes::new();
nodes.push(None, "/".to_owned(), dir, Children::Lazy);
TreeView::new(&nodes, |row| text(&row.data.name))
    .on_toggle(Message::Toggle)          // then nodes.toggle(&key); if nodes.needs_children(&key) { load; nodes.set_children(&key, kids) }
    .on_select(Message::TreeSelect)
    .selection(&self.tree_selection)
    .list(|list| list.on_activate(Message::OpenNode).height(400));
```

- `VirtualList::new(rows, build)`: fixed `row_height` (default 28), its own
  scroll offset and scrollbar, keyboard focus on click (`Focusable` for
  iced's focus operations), Page Up/Down by a screenful, Home/End,
  `on_key` for keys it leaves alone, `on_context` for a right press,
  `key(|i| ...)` for stable row keys so a row keeps its widget state when
  rows are inserted above it. `Selection` is sorted ranges with a cursor
  and anchor (`Mode::{None, Single, Multiple}`).
- `RowHeights::new(heights)` builds a reusable prefix index. Supply it with
  `.row_heights(&index)`; top offsets are constant time and visible-range
  searches are logarithmic. Rebuild the index only when caller data changes.
  `Columns::resizable_header` reports `Resize::{Preview, Commit, Cancel}`;
  apply preview widths to the same columns used for body rows, persist them
  on Commit and restore them on Cancel.
- `TreeView::new(&nodes, build)`: rows are the model's visible nodes
  (`Nodes::visible(row)`), each with guides and an expander (the icon
  font's `chevron_right`/`expand_more`, else a drawn box) before `build`'s
  content. Selection and activation are by visible row index.
- Styles: `virtual_list::Catalog` (`Style` with `Status::{Active, Hovered,
  Focused}`) and `tree::Catalog` (guides and expander), implemented for
  `Theme` from the tokens and for iced's theme.

## Native drag sessions

`dnd::native::Session<P, WindowId>` is a portable state machine. A `Codec<P>`
encodes one MIME payload; `Text` supplies UTF-8 text. The host feeds backend
events into `event`, executes `Effect::Request` on its native data device,
and applies `Effect::Delivery` to its target model. Then it calls `applied`
with the offer identity and whether application succeeded. Only a subsequent
successful source `Effect::Finished` authorises removing the source for Move.
Copy retains it. Failed, rejected, cancelled and closed windows never
produce successful completion. Feed pointer release to `released` and
window closure to `closed` so unused presses and offers cannot be reused.

Use `DragArea::on_native_drag` with a host event-to-token mapper and pass its
token to `Session::start_with_gesture`. This keeps the native press and widget
threshold in the same ordered event stream, even when asynchronous window
subscriptions have not delivered their messages yet. Backend validation still
rejects released, consumed and foreign-window tokens.

The session selects no window backend. A host adapter must supply real
held-press identities, native MIME/action negotiation, bounded nonblocking
transfer and the protocol's final completion event. It must reject stale or
wrong-window identities. A backend without these facilities should report
unsupported capability explicitly. Payloads are bounded to 16 MiB and MIME
names to 255 bytes. This API and the other library widgets compile against
pristine iced; an optional native host extension belongs outside the crate.

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

renders the gallery page offscreen (and `--test services` the dialogs and
toasts under Dark, Light and Custom tokens, plus keyboard-only runs of every dialog) (iced's headless simulator, software
renderer, embedded Fira Sans) under each token set and writes
`target/tmp/toolkit-snapshots/gallery-{dark,light,custom}.png` plus a
before/after pair for the live swap and the "Lists & trees" page as
`lists-{dark,light}.png`, checking the clear colour, a primary-filled
control, the selected row and that the sets differ. No window or GPU is
needed, so it runs on a build server.

## Taking it

- **Git dependency:** `toolkit = { git = "<this repository>", rev = "<rev>" }`.
  The crate resolves its iced crates through the workspace it lives in, so
  the pinned iced revision comes with it.
- **Vendoring:** copy `libs/toolkit/`, then point the `iced*` dependencies at
  your own iced 0.15.0-dev checkout (one path each; the crate uses
  `iced_core`, `iced_widget`, `iced_graphics`, `iced_renderer`,
  `iced_runtime`, optionally `iced_wgpu` and the `iced` umbrella for the
  examples), plus the ordinary dependency versions declared in the manifest.
- **No iced patches are required.** `TextField` owns its small input adapter
  over iced's public renderer/editor traits, including cursor restoration,
  secure masking and IME suspension. The library, gallery interaction tests
  and doctests are built against pristine iced `3de451447` in a separate
  workspace by the repository's `tests/toolkit/pristine_iced_gate.mix`.
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
