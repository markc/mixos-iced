# Edit service

`editd` registers the `edit` service and serves the `edit.v1` contract. Ced
and agents share its buffers through native ABP. The service is headless and
requires no compositor or D-Bus session.

`edit.open`, `edit.list` and `edit.get` discover buffers and page their
contents. `edit.insert`, `edit.delete`, `edit.replace` and `edit.apply` change
text. `edit.save`, `edit.reload` and `edit.close` manage disk state.
`edit.undo`, `edit.redo` and `edit.history` expose history; selection, cursor
and anchor verbs retain positions across edits. Properties have get, list,
describe and watch ports. Ask `HELP` for the complete manifest.

Mutations can carry origin, operation identity and expected revision.
Buffer identities include a daemon epoch, so a restarted service does not
silently reinterpret an old identifier. Paged snapshots remain tied to their
captured revision while later edits arrive. Slow work on one buffer does not
block operations on another.

Recovery defaults beneath `MIXOS_VAR/edit/recovery`.
`MIXOS_EDIT_RECOVERY_DIR` selects an absolute recovery directory; `0` disables
durable recovery. The session supplies its own state root. See
[the component](https://github.com/markc/mixos/blob/main/services/editd/README.md) and
[the shared contract](https://github.com/markc/mixos/blob/main/libs/edit/README.md).
