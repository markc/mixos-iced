# vendor/smithay: local patches

Smithay 0.7.0 from git master. It carries the local edits inherited with the
import and compd's own hooks.

## Upstream base: `7ddcd1736b47255ac209ddbc7c477fe69b4a9807`

`Smithay/smithay` master, 2026-08-17 13:44:43 +0200, "fix(tablet): deadlock when
using the tool handle in `with_tools`". The crate version there is `0.7.0`.

The import recorded no revision (there is no `.cargo_vcs_info.json`). The base
was recovered by measurement, as follows:

1. Clone `https://github.com/Smithay/smithay` (bare).
2. Write this directory into that repository as a tree object, without touching
   any branch. With a temporary `GIT_INDEX_FILE`, run
   `git --work-tree=vendor/smithay add -A` and then `git write-tree`.
3. For every master commit (first-parent) between 2026-04-01 and 2026-09-01,
   compute `git diff --numstat <commit> <tree> -- src` and sum the changed lines.
4. Pick the minimum.

The curve is monotone and has one clear trough:

| commit | date | `src` diff (lines / files) |
|---|---|---|
| `39cd5f19d0d1` | 07-27 | 2829 / 71 |
| `aa707b14aca1` | 08-10 | 2506 / 66 |
| `d54371145b17` | 08-12 | 1995 / 34 |
| `347b2b30225b` | 08-14 | 1785 / 29 |
| **`7ddcd1736b47`** | **08-17** | **1774 / 28** |
| `80ddb66d4de8` | 08-20 | 2327 / 51 |

The residual against the base is +1,574/−200 lines in 28 `src` files. That is
the inherited local delta. The table below is exactly that diff.

To re-check: `git diff 7ddcd1736b47 <tree> -- src` must list only the files below.

## Inherited local edits

Marked lines carry a `local patch:` or `local:` comment prefix (53 lines) and
are counted in the "marked" column. Many edits carry no marker at all; the
unmarked groups are selection, popup manager, DRM device and the
fractional-scale filter.

| file | +/− | marked | what |
|---|---|---|---|
| `xwayland/xwm/mod.rs` | +173/−23 | 7 | native Xwayland: EWMH support list trimmed (no HIDDEN/MAXIMIZED/MOVERESIZE/PING), OR-as-window, suspend no-op, selection |
| `xwayland/xwm/surface.rs` | +351/−38 | 2 | same: surface state for the native Xwayland path |
| `desktop/wayland/popup/mod.rs` | +158/−0 | 0 | **unmarked**: popup manager extensions |
| `desktop/wayland/popup/manager.rs` | +74/−6 | 0 | **unmarked**: popup manager extensions |
| `desktop/space/mod.rs` | +90/−10 | 0 | **unmarked**: `wl_output` enter/leave tracking |
| `backend/drm/device/{atomic,legacy}.rs` | +110/−63 | 0 | **unmarked**: DRM device changes |
| `backend/drm/device/mod.rs` | +25/−0 | 0 | **unmarked**: `DrmDevice::restore_state()`, the console-mode restore made callable (Drop is unreachable while the event loop holds the device) |
| `backend/drm/surface/atomic.rs` | +69/−17 | 4 | async (tearing) page flips |
| `backend/drm/surface/mod.rs` | +29/−0 | 1 | same: async flip plumbing |
| `backend/drm/compositor/mod.rs` | +40/−1 | 4 | same: never-empty frame / pacing |
| `backend/renderer/damage/mod.rs` | +19/−1 | 3 | `OutputDamageTracker::set_draw_all`: draw every reached element (for the shader pipelines) |
| `backend/renderer/element/utils/elements.rs` | +12/−2 | 0 | **unmarked**: crop fix |
| `backend/egl/ffi.rs` | +30/−0 | 0 | **unmarked**: `unset_debug_log()`, so EGL's post-`main` destructor logging cannot panic through dead TLS |
| `backend/libinput/mod.rs` | +10/−1 | 2 | tablet-pad events surfaced as `InputEvent::Special` |
| `backend/winit/{input,mod}.rs` | +83/−4 | 5 | pointer motion/leave surfaced from winit |
| `wayland/selection/mod.rs` | +31/−0 | 0 | **unmarked**: clipboard persistence, `SelectionHandler::selection_source_destroyed(ty, &Seat) -> Option<(mimes, user_data)>` |
| `wayland/selection/data_device/source.rs` | +31/−10 | 0 | **unmarked**: same; source destruction hands the selection to the compositor |
| `wayland/selection/offer.rs` | +9/−2 | 0 | **unmarked**: `send_selection(…, client: ClientId)` |
| `wayland/shell/xdg/handlers/surface.rs` | +11/−0 | 1 | xdg_toplevel lookup for the selection path |
| `input/dnd/grab.rs` | +18/−2 | 1 | drop_performed and cancelled are orthogonal (toplevel-drag) |
| `wayland/fractional_scale/mod.rs` | +32/−0 | 0 | **unmarked**: global `set_visibility_filter` (hide `wp_fractional_scale_v1` from Xwayland) |
| `wayland/text_input/text_input_handle.rs` | +112/−8 | 18 | OSK: surrounding text, sensitive flag, generation, relaxed request gate |
| `wayland/text_input/mod.rs` | +11/−3 | 1 | same |
| `wayland/seat/keyboard.rs` | +14/−9 | 3 | OSK: text-input `enter`/`leave` unconditional |
| `wayland/seat/touch.rs` | +8/−0 | 0 | **unmarked**: `TouchHandle::client_has_touch` (pointer emulation decision) |
| `wayland/xdg_toplevel_icon.rs` | +24/−0 | 1 | teardown fix |

Outside `src`:
- `Cargo.toml` enables `input`'s `libinput_1_26` feature next to `libinput_1_19`
  (tablet-pad dial).
- `smallvil/` holds an experimental winit example (+1,420/−42 across
  `smallvil` and two `anvil` lines). It is not built by compd.

A vendor delta audit is to turn each row into a behaviour test or a documented
drop.

## compd's patches (on top of the above)

| hook | where | what | guard |
|---|---|---|---|
| 1, filtered seat global | `wayland/seat/mod.rs` | `SeatGlobalData` carries a client predicate. `SeatState::new_wl_seat_with_filter` creates a seat visible only to accepted clients, through `GlobalDispatch2::can_view`. `new_wl_seat` keeps always-visible. `SeatGlobalData::visible_to` exposes the predicate | `wayland::seat::filter_tests::filtered_seat_global_is_visible_only_to_accepted_clients` |
| 2, `can_start_drag` (adapted to the local `dnd_requested` split) | `wayland/selection/data_device/{mod,device}.rs` | `WaylandDndGrabHandler::can_start_drag(&Seat) -> bool` (default `true`) is consulted first in the `StartDrag` arm, before the used-source record, the icon role and `dnd_requested`. Refusal cancels the client's source | `wayland::selection::data_device::can_start_drag_guard::start_drag_consults_can_start_drag_before_side_effects` (source guard: a behavioural test needs a live client with an implicit grab) |

compd uses them in `dispatcher`:
- the `agent` seat is created with a filter that hides it from
  Xwayland;
- `can_start_drag` refuses any seat other than the primary one.

Run EVERY guard this file cites (from the compd repo root):

```
mix tools/smithay_guards.mix
```

It builds this crate's own lib tests standalone with compd's feature set plus
`offline_test` (no `backend_vulkan`), and reports each guard PASS / FAIL /
MISSING. Standalone resolution is made to match compd's: `Cargo.toml` carries
the root's `[patch.crates-io]` (input, calloop) and pins winit exactly, and the
script seeds the (gitignored) `Cargo.lock` from compd's. Without that, a fresh
standalone resolution (2026-10-03) did not build: `WindowEvent::DragMoved`
missing from a later winit beta, and a non-exhaustive `backend::input::Axis`
from crates.io `input`.

### Selection hook, pre-request hook and local patches

**Hook 3: selection source gone + replacement.**
- `SelectionHandler::selection_source_gone(SelectionSource)` is named so that
  the existing `selection_source_destroyed` hook keeps its name. It has a
  default no-op, and is called first in all four `destroyed()` impls:
  data_device, primary_selection, wlr- and ext-data-control.
- The existing `selection_source_destroyed(ty, &Seat) -> Option<(mimes, ud)>`
  now runs through a shared `selection::replace_owned_selections`. It reads
  ownership under a scoped borrow, then asks the handler with no borrow held.
  It now covers the PRIMARY selection, and data-control sources owning either
  target; upstream cleared those unconditionally.
- Guard: `wayland::selection::hook3_guard::every_source_reports_gone_first_then_offers_replacement`.
  It is a source guard: in each of the four impls, `gone` must come before the
  ownership bookkeeping, and the source must go through the replacement path.

**Pre-request hook.**
- New `RequestInterposer::pre_request<I>(…) -> PreRequest`, with variants
  `Continue` and `Refuse`.
- New macro arm `delegate_dispatch2!(@interpose State)`. Every request of every
  Dispatch2-backed object passes through `pre_request` before its handler.
  Interposers pick their interfaces by downcasting `request`.
- Refusing a request that creates an object requires posting a protocol error
  first. This is documented on `Refuse`.
- The plain `delegate_dispatch2!(State)` arm is unchanged.
- Guards (`wayland::dispatch2::pre_request_tests`) drive a real raw-wire client
  over a socket pair: `interposer_runs_before_the_handler` and
  `interposer_can_refuse_a_request`.

**Local patches: DnD, session-lock, Xwayland/XWM, xdg-shell, input.** Every
hunk is marked `// compd: …`.

| patch | status | files | guard tests |
|---|---|---|---|
| DnD: wl_data_source destroy / client disconnect cancels a live drag | applied: a per-seat live-drag registry (the compositor builds the grab here, so it is not downcast), plus a dead-source backstop on release/motion | `input/dnd/{grab,mod}.rs`, `selection/data_device/source.rs` | `input::dnd::grab::cancel_suite_guard::*` (8) |
| DnD: touch cancel ends the drag (wl_touch.cancel, no drop) | applied | `input/dnd/grab.rs` | `touch_cancel_cancels_the_drag`, `touch_cancel_forwards_before_unset` |
| DnD: run-once finished/cancel | applied (`finished` flag) | `input/dnd/grab.rs` | `cancel_and_drop_run_at_most_once` |
| DnD: release-only drop, handler notified on cancel, slot check | upstream already has it (`should_drop`, `cancel()` → `cancelled`) | — | — |
| session-lock: surfaces bound to the accepted lock | partly upstream; `lock_object_may_create_surface` hook applied | `wayland/session_lock/*` | `get_lock_surface_validates_before_registering_output` |
| session-lock: validate before registering output; per-surface output registry | applied (the registry removes by owning surface; `retire_output_registration`, `locked_output_count`, `lock_surface_destroyed`) | same | `registry_removes_by_owning_surface_only` |
| session-lock: effective-buffer check | applied (deliberate change from upstream: an acked empty first commit is `NullBuffer`, as the protocol states) | same | `resolve_effective_buffer` tests (3: `empty_first_commit_has_no_effective_buffer`, `empty_commit_after_attach_keeps_the_retained_buffer`, `null_attach_clears_the_effective_buffer`), `pre_commit_hook_checks_the_effective_buffer` |
| session-lock: rejected lock cannot unlock / unlock once | upstream already has it (owner check, status set before unlock); guard added | same | `unlock_requires_the_owning_lock` |
| session-lock: Locking lifetime bound to the lock object | applied (a destroyed locking lock is done; late `SessionLocker::lock()` is inert) | same | `lock_destroy_aborts_locking_and_late_lock_is_inert` |
| XWM EWMH virtual desktops | applied behind `XwmConfig::with_ewmh_virtual_desktops` (default OFF, so `_NET_SUPPORTED` stays the trimmed list); `X11Wm::start_wm_with_config` | `xwayland/xwm/{mod,surface}.rs`, `xwayland/mod.rs` | `compd_ewmh_tests::*` (5), `set_desktop_mirrors_before_the_wire` |
| XWM `activate_request` | applied; it dispatches `_NET_ACTIVE_WINDOW` and defaults to forwarding to upstream's `active_window_request` | same | `ewmh_request_arms_are_dispatched` |
| XWM `set_active_window` | merged with the existing setter (kept its WM_STATE write); other-XWM windows publish NONE; override-redirect is a no-op (this WM focuses OR menus). Raw FocusIn/Out no longer write `_NET_ACTIVE_WINDOW` (compd publishes on every focus change) | same | `active_window_publication_is_managed_same_xwm_only`, `raw_focus_events_do_not_publish_active_window` |
| Xwayland orderly shutdown | applied (`begin_shutdown` on X11Wm and XWaylandClientData, classification, 2 s grace then reap off-thread) | `xwayland/xserver.rs`, `utils/x11rb.rs` | `shutdown_tests` (both files), `begin_shutdown_without_a_child_is_a_no_op` |
| Xwayland idempotent `disconnected` | applied | `xwayland/xserver.rs` | `vendored_xwayland_disconnected_stays_idempotent` (source shape) |
| offline X11 setters / `for_test` / keyboard-enter counter | applied, gated `#[cfg(any(test, feature = "offline_test"))]` (feature added to Cargo.toml) | `xwayland/xwm/surface.rs`, `xwayland/xserver.rs` | `compd_offline_tests::offline_setters_drive_the_real_getters` |
| xdg: a role from another protocol is refused; xdg role refused only while a live wrapper exists (Qt hide→show) | applied (per-wl_surface wrapper registry) | `wayland/shell/xdg/handlers/{wm_base,surface}.rs` | `destroying_the_xdg_surface_releases_the_wl_surface_for_a_fresh_wrapper`, `a_second_wrapper_while_one_is_live_is_refused`, `a_non_xdg_role_is_refused` |
| xdg: refuse get_xdg_surface with a buffer attached/committed | applied (`XdgShellHandler::surface_has_buffer` hook) | `wayland/shell/xdg/{mod,handlers/wm_base}.rs` | `…_pending_buffer_is_refused`, `…_committed_buffer_is_refused` |
| xdg: NULL-attach rule | applied; plus a compd fallback to `RendererSurfaceStateUserData` because compd's commit handler takes `current.buffer` (that fallback is untested) | `wayland/shell/xdg/mod.rs` | `an_uncommitted_null_attach_does_not_release_a_committed_buffer`, `smithay_default_buffer_check_ignores_an_uncommitted_null_attach` |
| xdg-decoration: destroy hook (chrome, 2026-10-03) | new: `XdgDecorationHandler::decoration_destroyed` (default no-op), called from the toplevel decoration's `Destroy`, so a compositor drawing SSD learns the client went back to CSD | `wayland/shell/xdg/decoration.rs` | `destroy_calls_the_decoration_destroyed_hook` (source shape) |
| libinput per-turn dispatch budget (opt-in) | applied (the local tablet-pad arm untouched) | `backend/libinput/mod.rs` | `backend::libinput::fairness_tests` (4) |
| keyboard: focus-to-None calls `focus_changed` | applied | `input/keyboard/mod.rs` | `focus_none_tests` |
| touch `cancel` unconditional | applied (upstream's frame-marker rewrite still drops cancel) | `input/touch/mod.rs` | `cancel_tests` (2); the per-client test fixture was fixed later: the implicit `TouchDownGrab` routes later downs to the first touch's focus, so the test returns to the default grab after each down (the implementation was right) |
| wlr_layer `reset_after_unmap` | applied, but keeps unacked configures (upstream's choice; clearing them would make a late ack a fatal serial error) | `wayland/shell/wlr_layer/mod.rs`, `wayland/compositor/cache.rs` | `reset_after_unmap_guard`, `cached_access_tests` |
| foreign-toplevel `new_toplevel_with_identifier` | applied | `wayland/foreign_toplevel_list/mod.rs` | `identifier_tests` |
| pointer `unset_grab_without_focus_restore`, `current_pressed` | applied | `input/pointer/mod.rs` | `compd_pointer_tests` (2) |
| compositor `transaction_applied` hook | applied | `wayland/compositor/{mod,transaction}.rs` | `transaction_applied_tests` (2) |
| `LibSeatSession::new_with_deferred_disable` | NOT applied (resumable KMS is a separate decision) | — | — |
| libinput touch `seat_slot()` (seat-wide touch ids; two touch devices no longer collide) | applied: all four touch event impls use `seat_slot()` | `backend/libinput/mod.rs` | `backend::libinput::seat_slot_guard::every_touch_event_uses_the_seat_slot` (source guard; a live proof needs two touch devices) |

### Later patches

**DnD acceptance is withdrawn when its target disappears** (2026-10-06).
`DnDGrab::drop` requires a live target surface. `WlOfferData::validated`
requires an active offer and at least one live `wl_data_offer` resource.
The native toolkit drag gate's target-close case exposed stale acceptance
after a receiver closed before release. The release now emits physical
drop-performed followed by cancellation, never successful completion.
Guard: `tests/desktop/toolkit_native_drag_gate.mix --case target-close`
drives the real primary-seat data-device protocol and waits for the target
generation to disappear before releasing the held button.

The offer destruction callback also retires abandoned transfers when the
receiver disconnects after Drop. Shared state counts outstanding offers and
cancels once when the last offer disappears without Finish. Destruction after
successful Finish never cancels. Guard: the same gate's `target-disconnect`
case transfers a large payload, kills the unacknowledging receiver and checks
that the source retains its Move payload.

**Lost page-flip recovery needs NO vendor patch.**
- A flip whose completion event never arrives leaves `pending_frame` set. While it is set, `queue_frame` parks the next frame and issues no commit, so the surface stays wedged.
- compd's stall rescue (`services/compd/crates/native/src/wire/frame/frame.rs` `rescue_pipe`) calls the stock `DrmCompositor::frame_submitted()` for a pipe that has been in flight for 2 s. That treats the flip as completed: the pending frame becomes current, and its buffer, which is the one on screen, is not handed back for reuse.
- A `forget_pending_frame` that dropped the pending frame instead was drafted and rejected. It would release the scanned-out buffer to the swapchain, and the next render could draw into the visible buffer.

**`_NET_WM_MOVERESIZE` advertised again** (`xwayland/xwm/mod.rs`).
- The inherited list left it out of `_NET_SUPPORTED` because interactive move/resize was refused by design (see the table above).
- compd handles the message (`XwmHandler::move_request` / `resize_request` start a real grab), so it is listed again. `_NET_WM_ALLOWED_ACTIONS` still decides per window.
- Guard: `xwayland::xwm::compd_ewmh_tests::default_net_supported_stays_trimmed` now asserts it is present.

**`_NET_WM_STATE_MAXIMIZED_HORZ/VERT` and `_NET_WM_STATE_HIDDEN` advertised again** (`xwayland/xwm/mod.rs`).
- The inherited list left them out because neither maximize nor minimize was implemented and there were no screen edges. compd answers both requests through its window policy, maximises into the usable area (the camera is pinned), and writes HIDDEN with `set_hidden_hint` (EWMH hint only; `WM_STATE` stays Normal) for a minimised or off-workspace window.
- Guard: the same `compd_ewmh_tests` guard; `mix tools/smithay_guards.mix` must stay green.

**Release-point signal failures are counted** (`wayland/drm_syncobj/sync_point.rs`).
- Every syncobj release-point signal goes through `DrmSyncPoint::signal`; smithay's callers (buffer release, merge, destruction, the cleared-pending path) only log a failure. A failed signal leaves the client waiting on a release that never comes, which compd counts as an explicit-sync fault.
- `signal` now routes its result through `note_signal`, which counts failures in a process-wide atomic; `drm_syncobj::signal_failures()` exposes the count. compd's `dispatcher` `explicit_sync::healthy` latches `info.explicit_sync_healthy` false on the first one and then refuses new explicit-sync commits. Behaviour of `signal` is otherwise unchanged (same result, same error).
- Guard: `wayland::drm_syncobj::sync_point::compd_signal_failure_tests::a_failed_release_signal_is_counted` (`note_signal` counts a failure and not a success; `signal` itself needs a DRM device, and its body is one `note_signal` over the ioctl).


### ext-idle-notify

**Timers armed through `IdleTimerLoop`** (`wayland/idle_notify/mod.rs`).
- Upstream's `IdleNotifierState<D>` takes a `LoopHandle<'static, D>`, where D is the Wayland dispatch state, and its timer closure reads `state.idle_notifier_state().is_inhibited`. compd's calloop data is `Wire<A>`, which is generic and is not `Dispatch`, and `dispatcher` (where the handler is implemented for `Dispatch`) cannot name it. So the state could not be built.
- `new` now takes `impl IdleTimerLoop`, a two-method trait (`arm`, `disarm`) that every `LoopHandle<'static, Data>` implements. Upstream callers that pass their own `LoopHandle<D>` therefore compile unchanged. The state stores a `Box<dyn IdleTimerLoop>`, plus `PhantomData<fn() -> D>` for the type parameter.
- `is_inhibited` is an `Arc<AtomicBool>` that each armed timer captures, so an expiring timer decides from it and never touches the loop's data. The closure's test is factored out as `fires(ignore_inhibitor, inhibited, is_idle_already)`, unchanged in meaning.
- Everything else is upstream: per-`wl_seat` notifications, `idled`/`resumed`, `notify_activity`, v2 `get_input_idle_notification` ignoring inhibitors, and a destroyed notification's timer left to fire on a dead resource (harmless, as upstream).
- Guard: `wayland::idle_notify::compd_idle_timer_tests::` covers three things. A timer armed on an `EventLoop<()>` (a loop with no state at all) fires exactly once. A disarmed one never fires. `fires` holds under an inhibitor, ignores it for input-idle notifications, and idles only once.
