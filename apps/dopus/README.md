# DOpus

Native Wayland twin-pane file manager, with headless navigation, sorting,
async listings and counts, safe file operations and configuration persistence.
Application control uses its existing `dopus.*` ABP verbs.

The app owns its renderer-free core, file operations and action registry as
private crates. The frontend uses the same iced and toolkit as Ced and Term.
Bundled SVG icons retain the Lucide and Feather licence and attribution.

Build and test on CBC at a pushed commit:

```
cargo test --locked -p dopus-core -p files -p actions -p dopus
cargo build --locked --profile release-fast -p dopus
```

Set the MixOS root to isolate application state from an existing desktop.
