# Term

Wayland terminal with a native banded CPU renderer, PTY tabs and panes, and
ABP Bus control. The default build uses tiny-skia; `--no-default-features
--features wgpu` selects the existing workspace GPU renderer. Neither profile
enables desktop D-Bus integration.

Configuration follows the shared MixOS directory rule:
`MIXOS_ETC`, then `$MIXOS/etc`, then the user or system configuration directory.
The file is `term.conf.mix`. The example belongs to `crates/term-core`.

The headless core and canonical broker fixture are private crates of this
component. Native session descriptor handoff uses the guarded child-only
teletypewriter patch in `vendor/teletypewriter`.

Build and test on an authorised CBC worker at a pushed commit:

```
cargo test --locked -p term-core -p term
cargo build --locked --profile release-fast -p term
```

The real native session and PTY acceptance tests additionally require the
fresh Mix binary, rather than relying on mocks.
