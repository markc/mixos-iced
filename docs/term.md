# Term

Term is the native Wayland terminal. Its toolkit-free core owns PTYs, the VT
model and native session control; the graphical app uses the shared toolkit
and the CPU renderer. It requires no D-Bus session.

Each tab contains one or more split panes. Ctrl+Shift+T opens a tab,
Ctrl+Shift+W closes it, and Ctrl+PageUp/PageDown selects adjacent tabs.
Ctrl+Shift+E splits horizontally; Ctrl+Shift+O splits vertically.
Ctrl+Shift+X closes a pane. Closing the final pane exits the app.

Ctrl+Shift+C copies and Ctrl+Shift+V or Shift+Insert pastes the clipboard.
Completed selections also populate PRIMARY; the middle button pastes it.
Shift overrides an application's mouse reporting. Selection and clipboard
use the compositor's native Wayland protocols.

Term registers `term`. Ask its `HELP` port for the current verb and argument
contract. `term.tabs` and `term.panes` report instance and pane identities.
`term.type` requires an explicit pane or tab; return the reported instance
token to guard against typing into a replacement process. Native allocated
control routes add incarnation, pane generation and request identity checks.

PTY children run `/opt/mixos/bin/mix`. A sealed native session descriptor is
handed to the child through the frozen `COSMIX_SESSION_FD` contract. Its bytes
remain compatible with existing nodes. See [native session](native-session.md)
and [the component](https://github.com/markc/mixos/blob/vt4-runtime/apps/term/README.md) for build and test details.
