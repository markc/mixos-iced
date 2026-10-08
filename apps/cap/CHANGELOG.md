# Changelog

## 0.1.4

- Complete GUI and headless descriptions from the shared presentation owner,
  including actual identity and installed resource/preparation/cache evidence.
- Refuse invalid raw description bodies on the existing bounded actor reply
  path before GUI admission; keep Cap's error vocabulary and string inventory.

## 0.1.3

- Use shared native admission, retained outboxes, origin-owning replies and
  bounded task sets on Cap's existing Bus worker. Reconnect retires stale
  commands before replacement admission; cloned requests share one reply token.
- Bound outgoing work through completion and reaping, with separate capacity
  for region cancellation and window restoration. Reply send budgets start
  when capture results exist, preserving long capture operations.
- Share bounded shutdown diagnostics and task retirement with BusViewer.

## 0.1.2

- Live settings over one nonblocking supervised connection with the shared
  paired bridge: prepared UI and mono typography, tokens and spacing render
  the chrome, and the settings lane drains before every Bus delivery against
  the live connection generation. Startup opens while the supervisor
  connects; an explicit typed name collision hands the launch paths to the
  running instance on Cap's existing worker (fenced by an untouched window
  that never registered), and a refused registration keeps the offline
  window open. Settings and cache evidence are available in `app.describe`
  and `cap.info`.
- The legacy theme-file reload is retired, and canvas and preview drawing are
  pinned to the explicit CPU renderer. Replies are tracked tasks fenced to
  the connection they arrived on; shutdown uses one bounded deadline for
  accepted replies, capture restoration calls, the settings cache and the
  client close.

## 0.1.1

- Ced-style shared File, Edit, Capture, Annotate, View and Help menus replace
  the rows of capture, file and annotation buttons, leaving the canvas clear.
- Capture targets, modes, delay and pointer choices use checked menu entries;
  annotation properties use a modal dialogue, also opened by the Text tool.
- Keyboard shortcuts, Alt menu mnemonics, F10 navigation and state-dependent
  actions share the menu dispatch path. Modal work is protected from agents.

## 0.1.0

- Initial native screenshot GUI and cap.v1 Bus service.
- Immutable source pixels, editable vector objects, undo/redo, crop, opaque
  redaction and no-clobber PNG export.
- Text and numbered annotations using the bundled font; bounded document
  history and coalesced preview work.
