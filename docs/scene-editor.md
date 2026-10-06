# Scene Editor

Scene Editor is a normal native Wayland app, using the same application host
and widgets as Ced. It has **File, Edit, Scene, Arrange, View and Help** menus.
The content area shows templates, installed scenes or panel arrangement,
with details beside the selection and a status strip below.

## Views and menus

- **Gallery** lists shipped templates and requirements. Select one and use
  File → Install Template or Install and Enable. File also offers the
  recommended set. Installing a template alone leaves it disabled.
- **Installed Scenes** shows state, source files and problems. Scene offers
  enable/disable, reload, sandbox, promote, move, reset and remove. Reset,
  promote and remove require confirmation and retain recovery copies.
- **Panel Arrangement** lists edges and pages. Select an edge or page and
  use Arrange to change mode, thickness or page order.
- **Edit** opens source in Ced through the native Bus. The scene editor stays
  open. The loader's diagnostics follow the file to Ced; validation and
  recovery remain owned by the scene service.

View selects Gallery, Installed Scenes or Panel Arrangement. Help contains
shortcuts and About. Invalid or busy actions are disabled.

## Keyboard

| Key | Action |
|---|---|
| Ctrl+R | Refresh |
| Ctrl+O / Ctrl+Shift+O | Edit scene / behaviour in Ced |
| Ctrl+1 / Ctrl+2 / Ctrl+3 | Gallery / Installed / Arrange |
| Ctrl+Q | Quit |
| F1 | Keyboard shortcuts |
| F10 | Activate menus |
| Alt+F / E / S / A / V / H | Open a menu |
| Arrows, Enter | Navigate menus |
| Escape | Close a menu or dialogue |

## Native operation

Launch `scene-editor`, its desktop entry, or `scenes.editor.open`. A second
launch activates the existing window and forwards any selection. No portal,
session D-Bus, shell relay or polling timer is used.

The loader now uses the app by default. `SCENES_EDITOR_APP=0` selects the
retained legacy scene frontend for recovery and its contract regression
suite. Editor templates and user copies remain on disk. `safe:true` is
accepted for normal activation; the compiled frontend does not execute an
editable editor behaviour.

`--service`, `--scenes` and `--comp` select service names; `--noded-url`
selects the native endpoint. `SCENE_HOST` selects the panel service/topic
namespace. Defaults are `scene-editor`, `scenes`, `comp` and `shell`.
`--version` reports source/build identity before configuration or Bus access.

## Bus contracts

The app serves `scene-editor.ping/info/show/action/quit`, `app.describe` and
`app.quit`. `info` reports selection, state token, complete snapshot,
busy/connection state and active dialogue. `show {view?,scene?}` restores and
raises the native window. Navigation and mutation are refused during a
confirmation or action; read-only queries remain available.
`action` accepts the loader's action arguments. An explicit, validated
`selection` is honoured; omitting it uses the app's current selection. An
omitted token uses its current snapshot. Query fresh state before confirming
an agent operation.

The loader serves:

- `scenes.editor.snapshot {selection?}` returns schema
  `scene-editor.snapshot.v1`, a `state_token`, selection, complete model,
  inventory, templates, panels and any host problem.
- `scenes.editor.action {action,state_token,selection?,confirmed?,edge?,mode?,direction?}`
  executes the existing scene planner. Selection contains `view`, `template`,
  `scene` and optional `page:{edge,page}`. Actions are `install`,
  `install-enable`, `recommended`, `toggle`, `reload`, `reset`, `remove`,
  `fork`, `promote`, `edit-scene`, `edit-behaviour`, `move`, `mode`, `size` and
  `order`. Size uses `minus/plus`; order uses `up/down`. Reset, remove and
  promote require `confirmed:true`.

The token covers authoritative inventory, templates and panels. Missing or
changed tokens refuse with `SCENES_STALE` and a fresh snapshot before mutation.
Confirmations keep their original identity and token; external changes require
reviewing new state before trying again.

Loader steps run directly through its reducer; panel/Ced/app steps use native
Bus. A later failure retains the planner's partial-progress reporting and
recovery semantics; this is not a distributed transaction. Completion replies
include a complete snapshot when available. Events request refetches; the app
does not guess deltas or automatically retry an uncertain mutating request.
An uncertain result remains in the status strip until a manual refresh or
navigation; a successful background read alone cannot prove that action's
outcome. First-run dismissal follows confirmed window activation, with one
pending launch completed by the app's registration event. Loader inventory
reports the frontend, registration and pending launch separately from the
retained legacy scene record.
An unconfirmed first-run launch ends with an explicit startup diagnostic after
a bounded deadline. It requires a manual open to retry and never loops launches.
