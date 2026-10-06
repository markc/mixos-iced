# DOpus Bus contract

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
