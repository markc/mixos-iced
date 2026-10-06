# editd

The native `edit` ABP service: shared text buffers for people and agents,
revision CAS, per-origin undo, anchors, atomic save, external-change detection
and a recoverable change feed. The buffer model is `libs/edit`, shared with
Ced; this service owns its actors, filesystem watching and recovery writer.

Discovery uses the shared node configuration. Recovery follows the MixOS Var
directory rule, or an explicit absolute `MIXOS_EDIT_RECOVERY_DIR`. Setting
`MIXOS_EDIT_RECOVERY=0` disables recovery explicitly.

Its retained integration tests exercise dispatch, recovery and the actual
service over a private native noded, rather than a substitute control socket.
