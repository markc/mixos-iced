# Shared application host

Ced, DOpus and Term share `libs/application` for native window startup and
renderer support. The host takes the initial state and boot task once, uses one
asynchronous task worker and supplies consistent window settings. Applications
add their own title, theme and subscriptions before running the event loop.

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

On a build worker, run `tools/application_gate.mix` to check that all three
components use the shared host and have no direct iced-family dependency,
including renamed, development and build dependencies. Unit and renderer
tests exercise the extracted controls. The desktop acceptance gate
`tests/desktop/application_native_gate.mix --bin-dir <candidate-binaries>`
uses an owned broker and nested compositor, exercises native keyboard input,
file copy/cancellation and real PTY output, and captures the apps at scales
1 and 2.5. It requires `bwrap` to bind the candidate shell at its canonical
path inside the test terminal's mount namespace. It leaves existing desktop
sessions and the host installation intact.
