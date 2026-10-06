# Ced

Ced is the native Wayland editor. The window keeps selection, tabs and view
state; `editd` owns buffers, edits, history, recovery and disk synchronisation.
Communication uses native ABP through noded. No D-Bus session is required.

Ced registers `ced` and exposes the `ced.v1` contract. `ced.open` opens a
path, `ced.tabs` lists tabs, `ced.state` reads editor state, and `ced.type`
and `ced.select` operate on the window's selection. `ced.actions` discovers
supported actions. `ced.diagnostics` and `ced.problems` expose scene linting.
`app.describe` describes the current application; `app.quit` exits it.

Bus edits retain caller provenance and use the same editor model as keyboard
input. Requests and refusal bodies are checked by committed golden fixtures.
The [edit service](edit.md) exposes buffer operations directly for agents.
See [the component](https://github.com/markc/mixos/blob/main/apps/ced/README.md) for its implementation and tests.
