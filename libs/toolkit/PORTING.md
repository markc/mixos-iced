# Porting and design notes

toolkit targets iced 0.15.0-dev at the revision vendored beside it, with
wgpu 30 in this workspace, or upstream's own renderer dependencies elsewhere.
These notes record what the port required and the generic API, so the
next iced refresh and anyone taking the crate can see what was deliberate.

## Features and layering

The default library has no window shell or selected renderer. The `wgpu`
and `tiny-skia` arms enable geometry for the audio canvases; the gallery
arms add the `iced` umbrella (winit on Wayland and X11, thread-pool
executor, embedded Fira Sans so the examples draw text with no host
fonts). The wgpu arm uses `wgpu-bare`, so the host chooses the GPU
backend. `tests/feature_graph.rs` holds these edges against the lock.

The library depends on nothing but the iced crates; `tests/generic.rs`
fails if a workspace crate or a path dependency outside `vendor/` enters
its normal-dependency closure, or if a file in the crate names the host
project.

## iced 0.15 adaptations

- iced removed `Widget::children`; mutable `diff` now initialises and
  reconciles TextField and context-menu child trees. Tests explicitly diff
  newly created trees, matching the runtime lifecycle.
- The shell uses iced's local message bus and carries window, waker, IME
  and clipboard requests. TextField uses `shell.local` and merges all
  requests.
- TextInput accepts borrowed fragments; the wrapper passes owned strings
  to retain its self-contained value and undo reconstruction. It refreshes
  that controlled fragment after edits, so later layout and events see the
  current value before the application rebuilds the widget.
- Text drawing explicitly keeps no ellipsis and no pixel hinting. The
  removed TextInput `icon` style field was unused; TextField offers no
  icon.
- TextInput state is generic over the renderer, which must be `'static`;
  undo snapshots store editor byte positions rather than grapheme indices.
- Clipboard reads are asynchronous: paste requests a read, and the text
  change arrives on a later clipboard event. Undo groups are split at both
  request and delivery; paste remains one undoable edit.
- iced's text editor captures empty IME preedit notifications; the menu
  still forwards them to its child, and the event status is Captured.
- iced 0.15 includes its own undo bindings. TextField intercepts them so
  its bounded, coalesced history stays authoritative, including the
  suppression during composition and window blur.
- Text-input tests use the real cosmic-text editor and shape text with
  iced's embedded Fira Sans (the `iced_graphics/fira-sans` dev feature),
  so `cargo test -p toolkit` needs no window, GPU or host font.
  Feature-graph assertions inspect normal/build edges only, excluding
  iced's self dev-dependency that selects upstream defaults for its own
  tests.

## Owned text input

`TextField` owns the adapter in `text_field/input.rs` and `text_field/raw.rs`,
adapted from the pinned upstream input widget with its MIT notice retained.
The editor engine is still iced's public `Renderer::Editor`. The adapter
keeps cursor/selection state available to bounded undo, synchronises secure
mask positions, and clears message tracking on immediate overwrite. Losing
focus cancels local preedit and blocks queued IME events until the platform
acknowledges closure. Password input requests the secure IME purpose.

No patched iced APIs are needed. The pristine-upstream gate copies only this
crate into an isolated workspace and fetches exact iced `3de451447`; it runs
library tests, rendered interactions and doctests there. Repeat that gate
when refreshing iced or the owned adapter.

## Theming

`Tokens { palette: Palette, metrics: Metrics }` is plain data with
`Copy`, so a widget's `style(...)` builder takes it by value and a view
closure can capture it. `Palette::dark()` and `Palette::light()` are
complete; the tests check AA contrast (4.5:1) on every text pair and the
UI threshold (3:1) on filled controls and the focus ring, so the defaults
read on their own. The widget styles derive from the palette:

- `text_input`: `surface`/`muted_surface` fill, `input`/`border`/`ring`
  outline by status, `metrics.border.width`, `metrics.radius.md`;
- `menu_style`: the `popover` pair, `selection` highlight,
  `metrics.radius.md`, `metrics.text.md`, with the default row metrics;
- `tooltip_style`: the `elevated` pair, so tooltip text never sits on the
  surface it covers, with `metrics.border.width`;
- `audio_style`: the `card` pair for strips, `primary` fill and notes,
  meter zones primary (below -12 dB), selection (to -3 dB) and
  destructive (above), `metrics.radius.sm`.

An application with its own theme fills the same fields; the crate parses
no theme format. Hard-coded colours remain only in `tokens.rs` (the
gallery's preview track colours and the menu's standalone default);
`tests/colours.rs` scans every other file under `src/` for colour
constructors and hex literals, with an empty allowlist and a planted
fixture that proves the scanner catches each kind.

### `theme::Theme`

`Theme { tokens, name, labelled }` is the iced theme type. Each iced
widget's `Catalog` is implemented with `Class<'a> = StyleFn<'a, Theme>`,
exactly as iced's own theme does, so `.style(|theme, status| ...)` and
`.class(...)` keep working, and the default class is the toolkit style
function for that widget (`theme::button::primary`,
`theme::container::transparent`, ...). The style functions read
`theme.tokens` on every call and hold nothing, so a theme swap is complete
on the next frame. Derived shades (hover, pressed) are `Color::mix` of two
palette colours, which keeps the colour gate green.

`theme::Base::name` returns `toolkit-<hash>` where the hash covers every
channel of every palette colour and every metric: iced's text editor
compares theme names to decide when to re-highlight, so the name must move
with the tokens. `Theme::named` keeps a caller's label instead (for a
picker) and so should only label distinct token sets.

The crate's own widgets take their colours from the theme through
`theme::Catalog` (`menu_style`, `audio_style`), implemented for `Theme`
and for `iced_core::Theme` (from its extended palette), unless given an
explicit style. Layout has no theme, so a theme-styled `Menu` or `Panel`
uses the default row metrics and only the colours and radius from the
theme; an explicit `MenuStyle` sets both. `Waveform` and `PianoRoll` key
their geometry caches on the style resolved at draw time (a `Cell` in the
widget state), so a swap redraws their cached bodies and tiles.
`TextField` is generic over the theme type with the same
`text_input::Catalog` bound as iced's `TextInput`.

`Theme::iced_seed`/`to_iced` map the palette onto iced's `Seed` (surface,
text, primary; success from primary, warning from selection, danger from
destructive) for third-party widgets that only style from `iced::Theme`.

### Snapshots

`tests/snapshots.rs` includes the gallery's `app.rs` by path, builds the
page element and renders it with `iced_test::Simulator` (the vendored
`iced/test` crate) on the tiny-skia renderer at 2x, which needs neither a
window nor a GPU. The test is `required-features = ["gallery-tiny-skia"]`
so a plain `cargo test -p toolkit` stays renderer-free. `iced_test` and
`png` are dev-dependencies only, so they never enter the library's
closure or the `generic` gate.

## Fonts and icons

`fonts::install(FontSet, Option<IconFont>)` is the whole registration
path. It reads every source first (bytes or path), identifies each face's
family from the bytes, then under one write lock on iced's font system
removes any preloaded face of the same family, loads the supplied faces,
checks each registered, and binds the generic sans-serif, serif and
monospace families to the sans, serif and mono roles. The application's
bytes therefore win over a system font of the same name, and widgets
using `Font::DEFAULT` or `Font::MONOSPACE` pick them up without an
explicit family. Display and emoji faces are registered for explicit use
(`Fonts::font(Role::Display)`) and script fallback.

Sources are validated before the installed-once rule applies, so a bad
path or malformed font is always reported; a second successful call is
`FontError::AlreadyInstalled`. Family names are interned so `Font` stays
`Copy`.

`font_for` resolves a family chain against the registered faces and
buckets a CSS weight to iced's named weights; a Light (300) request in a
family with no light face selects Normal (`effective_weight`), so a
fallback family never renders ExtraLight.

`IconFont::parse_codepoints` reads the `name hex` table format one line
per glyph, rejecting a bad row with its line number; `fonts::icon(name)`
returns the glyph and a font naming the icon family, ready for a text
widget, and `icon(name)` is that text widget (relative line height 1,
advanced shaping, colour inherited unless set). A missing glyph renders
the name so the gap is seen, not blank.

`icons::freedesktop` came from a compositor's scene host, where it was a
process-wide resolver reading the XDG environment. Here the directories
and theme are a `Lookup` value the caller builds (`from_xdg` is the one
constructor that reads the environment), the cache is a `Resolver` value
rather than a static, the host's application IDs are gone from the tests,
and the GTK/KDE theme-name discovery, the `-symbolic` round trip, the
shell-glyph aliases and the category fallback are kept as generic
freedesktop behaviour.

## Virtual list and tree

`VirtualList` is one widget that owns its scroll offset rather than a
child of iced's `scrollable`: a scrollable lays out its whole content, and
a hundred thousand row widgets would be built and laid out every frame.
Instead `layout` reads the offset from the widget state, builds only the
rows that cover the viewport plus a two-row overscan, reconciles the tree's
children by row key (a `HashMap` from the previous keys, so a one-row
scroll moves state rather than recreating it) and lays those out at their
absolute positions; `update` and `draw` only ever walk that window. A
wheel, key or scrollbar drag changes the offset and calls
`shell.invalidate_layout()`, so the next layout rebuilds the window; the
runtime's relayout keeps the same element tree, which is why the builder
closure lives on the widget. A `scroll_to_row` operation cannot ask for a
relayout (operations have no shell), so it leaves the request in the state
and the next event (the redraw the runtime schedules after an operation)
applies it. The test `hundred_thousand_rows_cost_a_screenful` counts
builder calls, layout nodes and kept child trees across a thousand scrolls.

Selection is caller state passed in and published out, as iced does with
text input values, so the list never diverges from the model; the widget
also updates its own copy on the way so a second event in the same batch
sees the first's result. Keyboard focus is taken on click and dropped on a
press outside, and the state implements iced's `Focusable` so
`focus_next` and friends treat the list as one stop.

`TreeView` adds nothing to the list's rendering: it is a builder that turns
the model's flattened visible nodes into list rows of `[Guides, content]`
and feeds Left and Right through the list's `on_key` hook, publishing the
caller's toggle or a new `Selection`. The model (`Nodes`) is an arena with
a key index and a lazily rebuilt visible list (`OnceCell`, cleared by every
mutation); the guides mask (one bit per ancestor depth with a later
sibling) is computed in that pass so a row draws its lines without looking
at its neighbours.

## Strings

The widgets draw only what the application gives them; `Item` labels and
accelerator strings are plain text. The gallery's labels come from
`i18n/en/toolkit.ftl` through `fluent-bundle`, a gallery-only dependency.

## Keys, dialogs and toasts

The key router, the lossless `Keys` capture, `Inert` and `FocusProbe`
came from two editors where the chord table named the applications'
actions; here `Bindings<A>` is generic over the action and keeps table
order, so an action's first chord is its accelerator label. Chords are
modifiers plus one key, spelled `Ctrl+Shift+S`; a letter is taken from the
Latin layout position (`Key::to_latin`) so the same chord works on a
non-Latin layout. F10 is left to the menu bar, which opens itself;
Alt+letter routes `Routed::Menu(index)` for `menu::open_operation`.

Dialogs follow the application-owned pattern: `Dialog` is state, `view`
draws it, `update(Event)` returns an `Outcome` when it is answered. iced
buttons are not focusables, so the dialog tracks keyboard focus itself as
a slot index (field or list, then the buttons) and draws the focus ring;
only the prompt's text input is an iced focusable, and the `Modal` frame
focuses or unfocuses it by running `operation::focusable::focus` or
`unfocus` on its layer whenever the dialog's declared focus target
changes. That keeps focus right on a fresh widget tree too, which is how
the simulator test can drive a prompt by keyboard. The frame takes Tab and
Escape before the layer (no text input uses them), everything else after,
and captures every key so nothing reaches the content under it; mouse
events stop at the opaque scrim. The sliding bar for indeterminate
progress asks for a redraw on every frame while it is on screen and
nothing otherwise.

Toasts keep per-toast deadlines in the `Toaster`; nothing in the crate
runs a timer. The overlay widget asks the runtime for a redraw at the
nearest deadline (iced's `request_redraw_at`) and publishes
`Event::Expired` when a redraw finds it passed, so a winit application
needs no timer executor; an application with its own clock calls
`sweep(now)` instead. The warning colour is derived (`destructive` mixed
towards the text colour) because the palette has no warning role; adding
one would change `Palette` for every taker.
