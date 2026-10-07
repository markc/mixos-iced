# Shared application host

Ced, DOpus, Term and Scene Editor share `libs/application` for native window startup and
renderer support. The host takes the initial state and boot task once, uses one
asynchronous task worker and supplies consistent window settings. Applications
add their own title, theme and subscriptions before running the event loop.

The optional `test-support` feature exposes the pinned UI simulator through
the host, so app tests need no direct iced-family manifest dependency.

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
