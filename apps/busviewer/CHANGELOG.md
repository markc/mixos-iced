# BusViewer changelog

## 0.1.3

- Observe installed frames through the shared application owner; opt-in
  fixtures use its inspector and preparation barrier on the existing actor,
  with bounded waits and separate control headroom.
- Retire installed-font discovery. The shared resource host owns text
  preparation; tree expanders use their explicit geometric fallback.
- Complete canonical descriptions from the shared presentation owner, including
  installed resource, preparation and cache evidence and actual native identity.
- Validate raw description requests before actor admission or whitespace
  normalisation; invalid requests cannot change frontend state.

## 0.1.2

- Adopt shared native task, reply and retained FIFO owners. Accepted credit
  survives reply completion until reaping; cloned GUI deliveries answer once.
- Declare inventory subscriptions on the existing supervisor. Settings wakes
  keep their queue position through floods; stale commands cannot mutate the
  frontend after reconnect. Retain the existing editor and singleton handoff.
- Bound outgoing admission, task storage and diagnostics. Queue time consumes
  the call/reply budget; shutdown reports unconfirmed cancellation truthfully.
- Apply prepared Menu, TextField and Tree styles through shared toolkit APIs.

## 0.1.1

- Adopt the shared settings pipeline: the existing Bus worker, runtime, client
  and receiver now also host the settings Lane, with nonblocking supervised
  startup and a settings cache under the busviewer app directories.
- The GUI borrows the prepared appearance from its settings session
  (bootstrap until activation); fonts are registered on the worker, and the
  legacy theme.changed subscription and frontend font install are gone.
- Prepared typography drives text, editors and lists; `app.describe` and
  `busviewer.info` report canonical settings and cache evidence.
- Settings frames are swallowed before topics; overflow accounts settings
  loss; registration collisions keep the existing show-handoff and never
  exit or forward after a successful registration. One shared shutdown
  budget drains replies, the settings cache and the connection.

## 0.1.0

- Port Cosmix BusViewer from Bevy/CTK to the shared iced application host.
- Native supervised Bus discovery, event-driven refresh and single-instance activation.
- Shared menus, service tree and split panes; multiline JSON input and selectable replies.
- Queryable app state, bounded calls and visible per-service discovery errors.
