# Shared application host

Ced, DOpus, Term and Scene Editor share `libs/application` for native window startup and
renderer support. The host takes the initial state and boot task once, uses one
asynchronous task worker and supplies consistent window settings. Applications
add their own title, theme and subscriptions before running the event loop.

The optional `test-support` feature exposes the pinned UI simulator through
the host, so app tests need no direct iced-family manifest dependency.

With `settings-native` or `acceptance`, `application::frames::Handle` observes the actual native
runtime without posting a widget message or requesting a redraw. Construct one
handle for each window incarnation and retain it. Bind its stable observer using
`Session::frame_stamp()` alongside the immutable view; this stamp describes the
installed content, including LastGood while a replacement is pending or failed.
The Iced builder's `frame_presentation` getter carries that binding through
layout and the exact buffer submission. An aborted submission cannot become
successful evidence when its native feedback later arrives.

The handle retains two copied metadata slots: latest status and latest presented
frame. `snapshot()` is read-only. `wait(Expected, deadline)` admits one waiter and
matches the exact window, activation epoch and applied local revision under a
caller-owned lifecycle fence and absolute deadline. It does not retry rendering.
Unsupported evidence is explicit; a settings ACK alone cannot satisfy the wait.
Native hosts must call `set_live_generation` before admitting requests and `close`
before draining shutdown work or replacing/removing a binding. Receipt history
does not prove current registration: Bus generation and settings authority are
separate evidence. Nested receipts describe nested presentation; they do not
attest physical scanout.

The non-default `acceptance` feature supplies a fixture-only Bus facade. A host
must also require an explicit owned fixture launch identity; enabling the build
feature alone does not authorise registration. All eight verbs are registered
under the shared application contract: `app.acceptance.describe`, `layout`,
`barrier.arm`, `barrier.wait`, `barrier.release`, `barrier.state`, `frame.state`
and `frame.wait` (each with the `app.acceptance.` prefix). Normal launches omit
these verbs. Every request carries `run` and `instance`; optional `generation`
must match the incoming native connection. Unknown fields and verbs are refused.
The owning actor retains admission until the result-bearing `track_result`
future and final reply complete and are reaped. The compatibility `track`
wrapper discards send errors; new hosts use the result-bearing entry point.

`acceptance::frames::Endpoint` shares the actual frame handle and one target
mailbox. The UI publishes its actual window id and installed stamp after window
creation and activation. `frame.state` returns this target separately from the
latest status and latest presented receipt. `frame.wait` additionally requires
numeric `window`, nonzero `activation_epoch`, `local_revision` and optional
`timeout_ms` (default 2000, range 1–10000). It captures a native lifecycle fence
at admission. A caller cannot supply that fence through JSON. An already
presented exact stamp may satisfy immediately; this does not promise a fresh
frame after the call. Discard, capacity or submission failure wait for natural
progress, while unsupported evidence, exhausted identities, closure, wrong
window, lifecycle loss, timeout and a second waiter fail explicitly. The final
reply has a separate two-second deadline and remains on its receiving socket.
Close frame, inspector and barrier owners before draining actor tasks; reserve
control capacity so a wait cannot block its own release or shutdown.

Portable controls live in `libs/toolkit`. `EditorPane` takes a bounded document
provider and emits editing intents; Ced adapts its editor engine to that
interface. `FilePane` takes a borrowed listing and a transfer bridge; DOpus
supplies filesystem state and operation policy. `TerminalPane` frames an
injected surface and emits scroll input; Term owns its PTYs, terminal state
and rasterisation. The standalone `compounds` example demonstrates all three
without importing an application engine.

The application host supplies software grid drawing and GPU grid painting.
GPU frame sources carry lifetime identities, retain their textures and upload
only damaged regions after the initial frame. Closing a pane retires its
texture. The portable toolkit selects neither renderer by default and also
builds against pristine upstream iced.

Applications keep their Bus connections, document buffers, filesystem jobs,
PTYs and shutdown order. The host adds no D-Bus dependency. Native sessions
use their own compositor and ABP broker.

Desktop appearance uses `application::presentation::native::{Session, Worker,
Mailbox}` with the host's existing supervised Bus client, runtime and incoming
receiver. `Session` owns UI-loop ordering and activation; `Worker` multiplexes
RPCs, deadlines and one coalesced resource job; `Mailbox` bounds pending settings
events behind one wake. `Worker::offline` prepares fallback resources while the
host's connection attempt continues. The live lifecycle/generation sample fences
every activation, including queued completions after connection loss.

Hosts with local zoom, output scale or painter coordinates use
`Session::with_context` and `Worker::contextual`. Requirements and content
preparation receive the same immutable context on the existing blocking lane.
`Ui::set_context` coalesces changes; equal contexts do no work. A checked local
revision fences old results, including failures and panics, before activation
or acknowledgement. Context does not require cloning or serialisation.

Local preparation retains the active settings snapshot and exact verified asset
binding. A generic fallback retains its already prepared appearance rather than
discovering a new set. Success runs the same synchronous activation hook without
refreshing settingsd, advancing its revision or writing another cache capture.
Failure keeps the previous presentation and exposes a separate local diagnostic
through `preparation_evidence`. `retry_preparation` retries that context without
retrying settings authority or cache writes. Hosts publish their latest context
before draining queued completions in the same UI turn. Legacy unit-context
constructors retain their two-argument callbacks; exhaustive event matches must
also handle the new opaque `Event::Resource` variant.

Ced and compd's scene-host use this path. Hosts that also activate renderer
resources use `Host::complete_with` or `Session::handle_with`: the synchronous,
total hook runs after successful preparation and current-stage fencing, before
the swap and acknowledgement. It must install prepared values without I/O,
awaiting or discovering unsupported inputs. Quoin uses it for panel preferences
and window chrome, including fallback activation.

`appearance::settings::Prepared`
supplies checked fonts, typography builders, toolkit tokens and validated button
read cells. Scene-host prepares decoration themes from the same dictionary and
selected title font. Appearance messages update existing scenes and corner menus
while retaining content, local edits, widget identity and keyboard state. New
surfaces borrow the current presentation. Compd retains its internal GPU renderer;
Ced retains tiny-skia. Panel geometry, complete control sizing/motion, persistent
host cache I/O and native first-map/frame acceptance remain in development.

On a build worker, run `tools/application_gate.mix` to check that all three
components use the shared host and have no direct iced-family dependency,
including renamed, development and build dependencies. Unit and renderer
tests exercise the extracted controls. The desktop acceptance gate
`tests/desktop/application_native_gate.mix --bin-dir <candidate-binaries>`
uses an owned broker and nested compositor, exercises native keyboard input,
file copy, deletion cancellation, real PTY output and native enrolment of both
Mix shells, and captures the apps at scales 1 and 2.5. It requires `bwrap`,
`setpriv` and non-interactive `sudo` to bind the candidate shell at its canonical
path inside the test terminal's mount namespace. Term runs as the invoking user
after privileges are dropped. Existing desktop sessions and the host
installation stay intact.
