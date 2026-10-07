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

## 0.3.10

Use shared native task admission and origin-owned replies on the existing Bus
worker. Retained send queue delay consumes the absolute reply budget. Replace
unbounded post-abort reaping with finite reports of unconfirmed cancellation;
the single two-second lane drain retains its separate bounded runtime teardown.
Real broker regressions cover expired replies and reused supervisor generations.

## 0.3.9

Add non-default owned fixture acceptance on the existing Bus worker. Explicit
run and process-instance configuration enables real root inspection, exact
frame waits and a barrier inside the actual blocking preparation operation.
Two tracked wait slots remain separate from ordinary control/reply capacity.
Actual window ids replace latest-window lookup. Ordinary launches omit the
fixture surface. Shutdown cancels observation and operation holds before drains.

## 0.3.8

Bind native frame feedback to the terminal's installed settings and local
preparation stamp. Retain the last good stamp while a replacement prepares.
The existing Bus worker fences waits on connection changes and closes the
observation owner before shutdown drains. Observation requests no repaint.

## 0.3.7

Prepare settings-owned terminal fonts, exact weights, scale and zoom on the
shared contextual worker. Install the complete painter and reconcile the
current pane tree and window geometry before acknowledging activation. Local
zoom and output scale reuse the activated source binding. Refused or stale
preparations retain the applied raster and terminal state. Compare PTY pixel
extents as well as rows and columns, and report desired and applied preparation
separately through `app.describe`.

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
