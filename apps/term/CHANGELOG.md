# Term Bus contract

## Unreleased

Reuse existing CPU pixels across terminal scroll rows and repaint exposed or
changed content. Preserve immutable retained bands, cursor repair and complete
moved-region presentation damage. Bound scroll detection on repetitive full
redraws and share glyph masks across foreground colours. Native session verbs
and wire contracts are unchanged.

Bound pane background rasterisation to damage through the shared tiny-skia
renderer. Retain unchanged row-band snapshots without allocating or copying
their cells, including cursor and font/viewport invalidation checks. Exercise
the complete themed TerminalPane with retained-buffer pixel comparisons.

## 0.3.5

Share the native application bootstrap and task worker. CPU/GPU selection and
raster diagnostics use host features; PTY ownership and ordered teardown stay
in the terminal. The existing native session contract remains unchanged.

## 0.3.4

Transplant the terminal frontend and native session verbs from the frozen
source revision `e0297242305f3a3c3de09f1ca01e8faa771768da`. The existing
cross-process session constants and child-only descriptor handoff retain
their bytes. The CPU grid renderer is ported to the workspace's iced 0.15
snapshot rather than adding another iced release.
