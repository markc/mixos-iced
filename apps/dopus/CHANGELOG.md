# DOpus Bus contract

## 0.4.5

- Adopt the shared desktop settings consumer: the windowed app presents
  fenced settingsd generations (checked appearance, complete synchronously
  prepared Lucide icon handles) through one `Ui`/`Lane` pair on the existing
  Bus worker, with a generic bootstrap before any installed-font I/O, a
  bounded incoming receiver, overflow coalescing and a single two-second
  shutdown budget. Replies, theme applies and the single-instance forward run
  as bounded owned jobs fenced to their accepted connection generation; the
  GUI launch performs no probe or forward, and `wait_done` returns the
  worker's authoritative shutdown receipt.
- `dopus.theme.set` and the `theme.*` actions are now fenced, validated
  `settingsd` mutations (`appearance.scheme`/`appearance.mode`) carrying the
  captured binding, incarnation, revision and a fresh operation id; without
  a confirmed read they refuse as unsupported — the legacy theme-file
  reload and in-session overrides are retired as live authority.
  Registration refusals use the typed rejection classification (a name
  collision counts only before the first successful registration).
- `app.describe` and `dopus.state` carry the reconciled consumer evidence
  and settings-cache status; the status bar shows the persistent
  provenance/fault labels from the shared Fluent catalogue. An initial
  refused duplicate never persists config over the registered owner, and
  the window keeps its offline look on fatal disconnects. Density, font,
  size and line height now key the layout/measurement caches; row/file-list
  and elide rendering still draw with default line heights — threading the
  prepared line heights through FileList/elide is the next shared-controls
  slice, not part of this change.

## 0.4.4

- Use the shared Noto Sans UI face at 300 and Material Rounded icons at 200.
  Preserve explicit authored text families and the missing-font icon fallback.
- Report UI and icon weights in the existing appearance state, with defaults
  when reading replies from older applications.

## 0.4.3

- Use toolkit's shared FilePane for row layout, cached drawing and stable click
  tracking; retain filesystem and native transfer policy in app adapters.

Use shared application hosting and renderer interfaces, including window
defaults, close deferral and the single task worker. Existing Bus bytes remain.

## Unreleased

Replace redraw-driven maintenance and the idle heartbeat with one cancellable
deadline wait. Config settling, metadata timeouts and chord expiry retain their
behaviour while a settled window stops redrawing.
Flush the latest config on shutdown and cancel pending chords on focus changes,
including unchanged and invalid keymap reloads.

## 0.4.2

Transplant the retained `dopus.*` verbs from cosmix
`e0297242305f3a3c3de09f1ca01e8faa771768da`, using the single workspace iced
snapshot and native ABP client. Its headless dependencies remain app-owned.
