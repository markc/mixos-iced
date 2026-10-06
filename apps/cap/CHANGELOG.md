# Changelog

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
