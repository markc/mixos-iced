# assets

The resolver for a pinned, immutable set of fonts, icons and emoji on the
local disk. No downloader, daemon or storage service: an installer
publishes a set as `<root>/sets/<id>` and points `<root>/current` at it;
this crate finds, checks and reads it.

## A published set

```
<root>/
  current -> sets/2026-10-04-core-3      the activation link
  sets/2026-10-04-core-3/
    manifest.conf.mix                    strict data, schema mixos.static-assets.v1
    fonts.css                            generated stylesheet, text locked as web_css
    fonts/  icons/  emoji/  web/  licences/
```

The manifest (`Manifest`) names the schema, the set ID, the font roles
(`sans`, `serif`, `mono`, `icons`, `emoji`, `display`, `serif_italic`,
`mono_italic`, …) and the family each role declares (`font_families`),
and locks every file (`AssetFile`: path, HTTPS url and upstream, revision,
licence, size, SHA-256, BLAKE3). The `icons` role's font has a
`.codepoints` catalogue beside it (`name hex` per line) that becomes the
`icon(name)` table.

Opening a set checks the layout (no symlinks, no escapes, no extra
components), the manifest against the rules in `manifest.rs`, every size
and the stylesheet text. `verify` streams every file through both hashes.
A set, once opened, is pinned to its `sets/<id>` directory: swapping
`current` changes what the next reader selects, not what this one holds.

## The generic API

Nothing in the core knows which project is asking. A `Lookup` is an ordered
list of asset roots the caller names:

```rust
use assets::{AssetSet, Lookup, XdgData};

// Roots from the XDG data directories (user first), then a system root
// from an environment variable with a default.
let lookup = Lookup::new()
    .xdg("example/assets")                                   // $XDG_DATA_HOME/…, each $XDG_DATA_DIRS/…
    .root_from_env("EXAMPLE_ASSETS", "/usr/share/example/assets");
let set: Option<AssetSet> = lookup.discover()?;              // first root with `current`, verified
let set: Option<AssetSet> = lookup.select()?;                // the same, sizes checked but no hashing

// Explicit inputs for a test, or a lookup assembled some other way.
let lookup = Lookup::new().xdg_in("example/assets", &XdgData::default()).root("/srv/assets");
let lookup: Lookup = vec![std::path::PathBuf::from("/srv/assets")].into_iter().collect();

// Reading.
let sans = set.font_path("sans");          // Option<PathBuf>
let family = set.family("sans");           // Option<&str>
let glyph = set.icon("delete");            // Option<char>
let css = set.file_path("fonts.css")?;     // Result<Option<PathBuf>>: only locked files resolve
set.verify()?;                             // sizes, SHA-256 and BLAKE3 of every file

// An installer opens its own root directly, before activating.
let set = AssetSet::open(root, "2026-10-04-core-3")?;
let set = AssetSet::current(root)?;        // follow `current` once; None when absent
```

Lookup rules: unset or empty XDG variables take their defaults
(`~/.local/share`, `/usr/local/share:/usr/share`); relative entries are
ignored; a root without `current` falls through; a root whose `current` is
dangling or whose set is malformed is an error, so a broken override is
reported rather than silently replaced by a system set.

The core depends on `strict` (the manifest parser), `serde`, `sha2`,
`blake3`, `hex` and `thiserror`. With `default-features = false` that is
all it depends on, and another project can take it as is.

## The MixOS defaults

`assets::mixos` (the default `mixos` feature) is the MixOS search path, kept
apart from the core the way `bus` keeps `noded_url()`:

| Order | Root |
|---|---|
| 1 | `$XDG_DATA_HOME/mixos/assets` (`~/.local/share/mixos/assets`) |
| 2 | each `$XDG_DATA_DIRS` entry's `mixos/assets` (`/usr/local/share`, `/usr/share`) |
| 3 | `assets` under `config::path(Dir::Share)`: `$MIXOS_SHARE/assets`, default `/opt/mixos/share/assets` |

```rust
let set = assets::mixos::discover()?;              // once, at startup, hashes verified
let set = assets::mixos::select()?;                // once, at startup, no hashing (compd)
let lookup = assets::mixos::lookup();              // the roots, for diagnostics
let lookup = assets::mixos::lookup_in(&xdg, &share); // explicit inputs
```

The lock and installer for the MixOS set live in `share/assets/`
(`core.conf.mix`, `install.mix`), which publishes to `/opt/mixos/share/assets`.

## Testing

`cargo test -p assets`. The unit tests cover the validators, the catalogue
parser and the lookup order; `tests/sets.rs` the public behaviour on
fixture sets in a temporary directory (pinning across a `current` swap,
XDG precedence, tampering, symlinks, escapes, bad manifests);
`tests/layout.rs` the installed `2026-10-04-core-3` layout rebuilt with
stand-in bytes and resolved through the MixOS search path, plus the real
installation when the machine has one.
