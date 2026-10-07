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

## 0.3.6

Adopt the shared desktop settings on the existing supervised Bus connection.
One client serves the `term.*` verb lane and the settings lane together; the
connection starts without blocking, so an offline bus leaves the terminal
working. An explicit name-taken refusal of the base name before anything
connects earns exactly one `<name>-<pid>` retry, and the suffixed name is the
one served. The tab strip's height follows the prepared UI line box plus the
strip paddings, computed once per wake and shared by the widgets, the PTY
grids and the IME cursor; token (colour) changes restyle without reflowing or
re-rasterising, and PTY fonts, zoom and ANSI colours are untouched. The
`term.*` wire verbs, the completion-note drain and the tabs-first shutdown
ordering are unchanged. `app.describe` now reports the actual served name,
the full serialized settings evidence and cache state, the chrome extent and
the prepared UI typography.

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
