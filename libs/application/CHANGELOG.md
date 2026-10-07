# Changelog

## 0.1.11

- The central verified resource host takes over live worker preparation: one
  serial blocking job reads the authored or recorded resource identity once,
  verifies the bytes, decodes required image variants and registers exactly
  one atomic toolkit batch. The exact binding of what was verified is
  acknowledged with the synchronous activation fence, so the captured cache
  save records precisely what was prepared. `Worker::with_resource_requirements`
  adds an application's pure icon requirements; `Worker::offline_with_host`
  and `Worker::offline_with_cache_and_host` accept an explicit host policy for
  tests and embeddings. Fallback preparation carries its verified binding
  through the ladder. The legacy `prepare_registered_checked` compat path
  remains available; the worker no longer uses it.

## 0.1.10

Paired cache evidence explicitly reports an unconfigured persistent root,
separately from write faults, saved identities and usable fallback state.

## 0.1.9

- Pair existing settings Session/Worker through owned UI and worker endpoints.
  Share coalesced jobs, bounded mailbox delivery, cancellation-safe worker
  progress and latest-watch cache flushing on the host's existing runtime.
- Ced and Quoin use the pair, keeping connection lifecycle, host wake delivery
  and shutdown policy under their existing owners.

## 0.1.8

- Share serialisable cache persistence, fault and fallback evidence between
  native applications and the shell, preserving existing Ced readback fields.

## 0.1.7

- Share explicit native `Renderer`/`Element` defaults so software apps retain
  tiny-skia when another workspace consumer enables the compositor's GPU path.

- Expose the latest producer-fenced settings cache persistence identity.
  Written and unchanged saves provide a receipt; superseded or foreign work
  cannot claim the current presentation was persisted.

## 0.1.6

- Opt-in `settings-cache` loads resource-checked persistent fallback and saves
  activated captures on the existing single blocking worker lane. Coalesce
  saves, retain writer locks after errors and through reconnect, fence reports,
  and allow explicit retry and bounded worker shutdown drain.
- Start fallback resource work during authority bootstrap, including builds
  without caching. Preserve fallback diagnostics after a successful embedded
  activation. This does not establish a GUI first-map timing guarantee.
  Explicit refresh retries failed offline fallback resources; ordinary wakes
  remain quiescent.

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
