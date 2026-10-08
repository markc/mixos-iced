# DOpus

Native Wayland twin-pane file manager, with headless navigation, sorting,
async listings and counts, safe file operations and configuration persistence.
Application control uses its existing `dopus.*` ABP verbs.

The app owns its renderer-free core, file operations and action registry as
private crates. The frontend uses the same iced and toolkit as Ced and Term.
Bundled SVG icons retain the Lucide and Feather licence and attribution.

Appearance comes from the shared desktop settings authority: the windowed app
presents fenced `settingsd` generations through one `Ui`/`Lane` pair on its
existing Bus worker (checked prepared appearance plus complete,
synchronously prepared Lucide fallback icons), with a generic bootstrap
before the first activation and a persistent provenance/fault status line.
`dopus.theme.set` and the `theme.*` actions are fenced, validated settings
mutations; the legacy `theme.conf.mix` files are not a live authority. The
settings cache lives under the app cache directory in `cache/settings`, and
replies/theme applies/handoffs are bounded owned worker jobs fenced to their
connection generation — the GUI launcher never probes or forwards at launch.

Prepared density, fonts, sizes and line heights key the layout/measurement
caches; row/file-list and elide text still renders with default line heights
(the prepared line heights ride the theme for the next shared-controls
slice).

Build and test on CBC at a pushed commit:

```
cargo test --locked -p dopus-core -p files -p actions -p dopus
cargo build --locked --profile release-fast -p dopus
```

Set the MixOS root to isolate application state from an existing desktop.
