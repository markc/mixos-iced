# Desktop icons

MixOS ships a small offline `mixos` icon theme for the panel, launcher and its
four core apps, in 400, 300 and 200 weights. These are unmodified Material
Symbols Rounded SVGs at the same
pinned Google revision as the symbol font in `share/assets/core.conf.mix`.
The SVGs scale with the output and their `-symbolic` names make scene-host use
the current foreground colour. They need neither a host theme nor D-Bus.

`material.conf.mix` version 1 records each source URL, byte count and SHA-256,
plus upstream revision and licence. `tools/material_icons.mix --check` verifies
it; `--record` refreshes it deliberately after importing changed source files.
Google's Apache-2.0 licence is retained under `material/LICENSE`.

Install the theme and the canonical M mark into the same share directory:

```text
mix share/icons/install.mix --root /opt/mixos/share
mix share/brand/install.mix --root /opt/mixos/share
mix share/icons/install.mix --root /opt/mixos/share --verify
```

The brand installer places the M SVG directly in MixOS and in inherited hicolor.
It supplies the missing hicolor index in a fresh image and preserves existing
theme metadata. The apps service selects the installed MixOS theme by default;
explicit user theme preferences are preserved. The panel requests symbolic
status names and prefers the M over legacy launcher
icons. `apps.reload` clears theme misses when assets are installed while running.

All three weights are installed together: `mixos` retains the original 400-weight
files and remains the default; `mixos-300` and `mixos-200` supply the thinner
sets and inherit the same M logo. Select a set through the existing
`apps.icon_theme` property (`apps.props.set` with
`{path:"apps.icon_theme",value:"mixos-200"}`, `"mixos-300"` or `"mixos"`).
The panel status icons, task icons and launcher app icons follow that choice.
An explicit `PANEL_ICON_THEME` overrides only the panel status/brand theme.
The property change lasts for the current apps citizen generation; for a
persistent preference, set `[Icons] Theme=mixos-200` (or your chosen theme)
in `~/.config/kdeglobals`.

`mix tools/icon_comparison.mix` writes a labelled side-by-side SVG under
`target/mixos/icons/`, with all three sets at identical sizes and colours.
