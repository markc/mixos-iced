# Port of cosmix-iced-widgets 0.1.7

Source: cosmix `src/desktop/crates/cosmix-iced-widgets`, revision
`685622493a5203635661fb9adc1cea4a4fc4ac3b`. Toolkit inherits compd's version
and edition and uses only its vendored iced 0.15.0-dev and wgpu 30.

## Cosmix pin

The existing workspace pin is retained for every cosmix dependency, including
design and the newly added assets resolver. Contrary to the brief's chronology,
`f92f9e78`, `549fea4b` and `4799db04` are ancestors of `68562249`.
That revision already contains the verified Material Symbols catalogue and
shared installed-font registration. A pin bump is unnecessary.

## Features and layering

The default library has no window shell or selected renderer. The `wgpu` and
`tiny-skia` arms enable geometry for the audio canvases; gallery arms enable
the vendored iced application shell with Wayland only. The wgpu arm uses
`wgpu-bare` and the workspace's GLES backend, matching compd's GPU stack.
The naga 27 termcolor workaround is dropped: this graph uses vendored naga 30.
The gallery brings iced's exact web-sys 0.3.85 requirement into the lock;
Cargo consequently aligns the wasm-bindgen family to that version.

Workspace widget renderer selection moves to its existing engine consumers
without changing their enabled features. Toolkit is layer 1, with no engine
crate dependencies, below cos-iced (3) and cos-scene-iced (5). Neither host
depends on toolkit in T0.

## API adaptation and behaviour

- iced removed `Widget::children`; mutable `diff` now initialises and
  reconciles TextField and context-menu child trees. Tests explicitly diff
  newly created trees, matching the runtime lifecycle.
- Shell uses iced's local message Bus and carries window, waker, IME and
  clipboard requests. TextField uses `shell.local` and merges all requests.
- TextInput accepts borrowed fragments; the wrapper passes owned strings
  to retain its existing self-contained value and undo reconstruction. It
  refreshes that controlled fragment after edits, so subsequent layout and
  events retain the current value before the application rebuilds the widget.
- Text drawing explicitly keeps no ellipsis and no pixel hinting, matching
  the source controls. The removed TextInput `icon` style field was unused
  by TextField, which does not offer an icon.
- TextInput state is now generic over the renderer, which must be `'static`;
  snapshots store editor byte positions rather than 0.14 grapheme indices.
  The narrow vendored state access patch is documented in iced's PATCHES.md.
- Clipboard reads are asynchronous in iced 0.15: paste requests a read,
  and the text change arrives on a later clipboard event. Undo groups are
  split at both request and delivery; paste remains one undoable edit.
- iced's text editor captures empty IME preedit notifications; the menu
  still forwards them to its child, but the event status is now Captured
  rather than Ignored. The ported test asserts that new status.
- iced 0.15 includes its own undo bindings. TextField intercepts them so
  its existing bounded/coalesced history remains authoritative, including
  the existing suppression during composition and window blur.
- Text-input tests now use the real cosmic-text editor; the 0.15 no-op
  editor no longer implements text entry. Feature-graph assertions inspect
  normal/build feature edges, excluding iced's self dev-dependency that
  selects upstream defaults for its own tests and benchmarks.

No new widget features are introduced.

The gallery's existing English strings are in `i18n/en/toolkit.ftl`; only its
title changes to the new crate name. Preview and menu-default colours move
to the token path without changing their values. Fluent is gallery-only.
