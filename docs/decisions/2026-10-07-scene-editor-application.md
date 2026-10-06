# Scene Editor becomes a native application

Status: accepted by the request to make Scene Editor a normal app with
Ced/Cap-style menus, 2026-10-07.

`apps/scene-editor` is a native Wayland app using the shared `application`
host and generic `toolkit` controls. It has its own decorated window, menus
and single-instance native Bus activation.

The scene loader retains installation, persistent enablement, validation,
rendering and recovery. Additive snapshot/action verbs reuse the shipped
editor's pure model/planner. Loader steps run through its reducer without
recursive self-RPC; other service steps use native Bus. Selection and
confirmation belong to the app. State tokens guard mutations against stale
snapshots, including external changes while a confirmation is open.

Normal `scenes.editor.open/close` activation addresses the app. An explicit
`SCENES_EDITOR_APP=0` process setting retains the legacy frontend for recovery
and contract tests. No user data, template or frozen wire constant is removed.
No alternate transport, D-Bus requirement or MixOS dependency in generic
toolkit is introduced.
