# DOpus

DOpus is the native two-pane Wayland file manager. Navigation, selection,
sorting, file-operation dialogs and drag and drop use the shared toolkit.
It requires no D-Bus session.

Its Bus service is `dopus`, with the `dopus.v1` contract. `dopus.state` reports
both panes; `dopus.open` navigates a selected pane. `dopus.actions.list`
discovers navigation and view actions, and `dopus.action` invokes them.
`dopus.theme.set` changes the selected theme and `dopus.quit` exits.

The inherited contract refuses `file.*` actions on the Bus. File creation,
copy, move, rename and deletion use the window's keyboard or pointer actions
and confirmation dialogs. This is a retained application contract, rather
than a replacement transport. See [the component](https://github.com/markc/mixos/blob/main/apps/dopus/README.md)
for the exact request types and tests.
