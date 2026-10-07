# Changelog

## 0.1.5

- Add synchronous `Host::complete_with` and `Session::handle_with` activation
  hooks after the existing stale/live fence and before applied acknowledgement.
  Hosts install total, prepared UI state in the same turn; previous methods
  remain supported and use a no-op hook.

## 0.1.4

- Box the private preparation result so queued events stay small when prepared
  appearance data grows. Public completion/event constructors, activation and
  fencing are unchanged.
- Quoin uses the same borrowed native worker and presentation coordinator for
  scene content, menus and decoration. Geometry and native frame proof remain
  pending.
- Add `wgpu-bare` for hosts that select GPU backends themselves. Quoin keeps its
  existing GLES backend without enabling Vulkan through application hosting.

## 0.1.3

- Opt-in `settings-native` centralises UI coordination and multiplexed jobs on
  the host's existing Bus worker. Coalesce RPCs, use deadline wakeups, and bound
  blocking resource work physically to one running job and one latest capture.
  Check the live connection generation on the UI loop before activation.
- Share take-once completion messages and a bounded settings mailbox. Snapshot
  queue gaps trigger reconciliation. No connection or runtime is created.
- Embedded fallback uses the same resource preparation and activation fence.
  Host cache I/O, cold first-map timing and Quoin integration remain pending.

## 0.1.2

- Opt-in `settings` shares captured worker preparation and synchronous fenced
  whole-presentation activation/acknowledgement, including deliberate app content.
  Superseded work cannot swap; resource failures retain the last presentation.
  The bridge owns no transport, runtime, receiver or timer.

## 0.1.1

- Opt-in `test-support` exposes the pinned UI simulator to app tests through
  the shared host, keeping app manifests free of iced-family dependencies.

## 0.1.0

- Share native bootstrap, take-once state ownership, single task worker, window
  configuration and renderer feature selection across Ced, DOpus and Term.
- Share native CPU grid drawing and persistent GPU textures over caller-owned
  frame sources. Retain sparse damage and explicit frame lifetime identities.
