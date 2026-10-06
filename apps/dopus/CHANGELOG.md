# DOpus Bus contract

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
