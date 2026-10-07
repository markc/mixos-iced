# Porting Bevy apps to iced

This guide records techniques used to port BusViewer from Cosmix Bevy/CTK to the MixOS shared application host. Scene Editor supplies related menu, modal and native activation patterns. It is a working method for Tower, InteractGUI, Media and Email; their backend requirements must be assessed separately.

## Keep the behaviour, change the scheduler

Read the old model, backend, rendering systems and tests before editing. Record source revision and paths. Separate protocol behaviour from Bevy types: remove `Resource` from plain state, replace entity IDs with stable domain keys, and retain wire names, return codes, argument validation and third-party attribution. Move existing tests to the new architecture; do not discard behaviour because a test used Bevy.

| Bevy / CTK role | MixOS iced replacement |
|---|---|
| `Resource` model | Plain Rust app/domain state |
| Chained Update systems | One reducer handling typed `Message` values |
| Observer callbacks | Widget messages and reducer actions |
| Startup system | Shared host bootstrap plus initial task |
| Channel drain each frame | Subscription waking on native deliveries |
| Background network system | App-owned supervised Bus worker |
| Entity selection | Stable domain key, mapped to current tree row |
| Despawn/rebuild UI entities | Declarative `view`, preserving widget tree identity |
| Reactive frame intervals | Damage-driven host, no periodic refresh timer |
| CTK theme/font setup | `appearance::install`, shared tokens and fonts |
| DCS shell around content | Normal menus and client content |

BusViewer separates `model.rs`, `bus.rs`, `menu.rs`, `strings.rs` and `app.rs`. This separation does not imply every app needs five files or that domain code belongs in the toolkit.

## Use the shared host and existing compound controls

New ports depend on `application`, `toolkit` and `appearance`, without direct iced-family dependencies. The host exposes the pinned iced API, a bounded task executor, renderer features and native window defaults. The app still owns its backend worker and graceful shutdown. The shared-host gate currently covers Ced, DOpus, Term, Scene Editor and BusViewer. Cap's earlier frontend still needs its image and clipboard adapters migrated into the application host; its conventional menus can be reused as a UI reference.

Use `TreeView`/`Nodes` for keyed, virtualised trees, `Split` for resizing, `TextField` for search, `Menu` for conventional menu bars and `CenteredButton` for dialogue controls. Multiline JSON editing uses the pinned `text_editor` through the host; specialised source editing, files and terminal grids use the existing shared `EditorPane`, `FilePane` and `TerminalPane` contracts. Promote a new compound control only when another owner needs the generic interaction, with an API review. Service discovery and application commands remain app semantics.

Use appearance tokens for spacing, type, surfaces and focus styling. Use appearance's UI and mono fonts. Do not embed a private font/icon setup in each port. Desktop font and icon weight preferences should flow through the same appearance path as Ced, DOpus, Term and Cap.

## Make asynchronous boundaries explicit

Use one supervised native Bus connection registered under the app's service identity. Forward incoming commands, relevant topics and connection state into a subscription. Bound pending commands and outgoing concurrency. The backend does no rendering and the view does no network work.

Give every operation a monotonically increasing ticket. Check a completion's ticket before clearing busy state or applying its result. Where results are tied to navigation, also fence the selection epoch. BusViewer discovery describes all services, so it reconciles a stable selected `(service, verb)` against the completed snapshot instead.

Freeze a call's service, verb and body at acceptance. Store the reply against that frozen request even if the user selects another row. Reject duplicate activation while a call runs. Never replay a mutation after timeout or disconnection: the receiver may already have applied it. Report an uncertain outcome and allow an explicit new user action.

Discovery uses non-fail-fast probes. `HELP` can fail or return an old schema; try documented `app.describe`, and preserve both failures if neither works. Bound concurrent descriptions and show failures per service. Unknown read-only status stays unknown. Preserve plain-text error bodies alongside return codes; JSON parsing is for content that is actually JSON.

Coalesce native registry events into one pending refetch while busy. Filter props topics to the paths the app consumes; unrelated props must not trigger a discovery loop. Reseed on reconnect or topic gaps. A failed broker lookup is not evidence a selected verb disappeared: retain the last successful snapshot and show the failure. Do not add a periodic timer to cover a missing event: extend the owning native service when an event is needed. BusViewer retains manual mesh refresh because noded currently exposes membership through `noded.peers` without a membership-change topic; automatic authority updates are an owning noded requirement for the later Tower port.

## Preserve input and widget state

Keep the same `Modal::host(base, optional_layer)` at the same tree position whether a dialogue is open or closed. Changing the outer wrapper discards child input/menu state. Block keyboard and agent actions behind a modal as well as pointer clicks. Give a visible, reachable Done/Cancel control at the app's minimum window size.

Tree selection belongs to domain keys; row indices change when search or discovery changes. Preserve expanded keys and reconcile removed selections. Read-only result editors accept navigation, selection and copy actions while rejecting edit actions. Enforce byte bounds at input acceptance and use UTF-8 boundaries for truncation.

Menu labels, accelerator text, keyboard shortcuts and mnemonic navigation must agree. Test F10 and Alt mnemonics as well as pointer clicks; global shortcuts must not bypass the modal or busy reducer checks.

## Native identity and lifecycle

Use a single-instance startup probe and forward activation to the running app. Registration must reject a competing instance; a race loser can probe and forward again. Implement `ping`, `info`, `show`, `quit`, `HELP`/`app.describe` as an explicit contract and register app verbs.

Showing a window waits for compd's mapped event, finds the app's own PID, and addresses `{id, generation}`. Restore minimisation, then raise/focus. Do not assume title or app ID alone identifies a unique window. Closing waits for accepted work; an automated quit can refuse busy state.

## Verification that survives the port

1. Pure tests cover protocol schemas, legacy descriptions, partial failures, input validation, raw errors and UTF-8 bounds.
2. Reducer tests cover duplicate calls, stale completions, changed selection, unknown outcomes, explicit caller targets and modal blocking.
3. The shared host's simulator covers menus, minimum size and reachable dialogue controls using the actual widget tree.
4. An isolated real broker verifies registration, discovery, rc/body preservation, topics and reconnects. Fixtures stand in for external citizens, not for the Bus transport. Keep their schemas strict enough to match the real contracts.
5. Build, Clippy and dependency/layering/native-session gates run on the exact pushed source revision. Preserve lockfile versions outside the port.
6. On the real VT desktop, verify the executable hash/provenance, launcher icon, discovery and read-only call, singleton activation and a captured settled frame. Preserve existing windows, tabs and user files.

A Bus receipt can arrive before the next Wayland frame. Wait for presentation or a settled frame before judging a screenshot. Native synthetic input must include realistic press/release timing; a too-short click can miss an app frame. Keep test timing at the acceptance boundary, not as an app redraw timer.

## Following apps

**Tower:** keep mesh inventory and topology as plain state. Use native events for updates. Existing SSH-driven service controls need owning noded lifecycle verbs before the controls can ship; they cannot become a second control transport.

**InteractGUI:** preserve lease, reseed, reconnect, progress and result contracts. Presentation is a keyed task/dialogue model, not Bevy entities. Test multiline and multi-choice input, cancellation and reconnect recovery explicitly.

**Media:** keep decoder/audio clocks off the GUI thread. Use bounded latest-frame delivery and invalidate only when content changes. Review backend licences before importing dependencies; replacing the GUI does not solve a backend licence issue.

**Email:** the old Mail frontend uses fixture storage. A real port requires native mail backend integration and state tests; moving its widgets alone does not produce a usable mail client.

**Studio:** assess its rendering, timeline and audio architecture separately. Share suitable compound audio controls, while retaining app-owned scheduling and domain semantics. BusViewer's relatively small port is not a size estimate for Studio.
