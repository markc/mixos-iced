# Cap

Cap takes screenshots through compd and edits a separate annotation document.
Its iced window uses the shared toolkit and Fluent catalogue. Both its GUI and
agent commands use the native ABP Bus. A D-Bus session is unnecessary.

The window has **File**, **Edit**, **Capture**, **Annotate**, **View** and
**Help** menus above the preview canvas, with status and current tool below.
The menus use the same shared toolkit control as Ced. F10 activates the bar;
Alt+F/E/C/A/V/H opens a menu, and arrows, Enter and Escape navigate it.

Under **Capture**, choose **Mode → Full screen**, **Window** or **Region**,
select an output or window, set a delay of 0–10 seconds and choose whether to
include the pointer. Checked entries show the current choices. **Capture →
Take screenshot** (Ctrl+N) keeps Cap visible during the delay so you can cancel
with **Capture → Cancel** or Escape. Cap then
asks compd to minimise its own fenced window, captures a fresh frame and
restores itself. Region mode uses compd's selection overlay; Escape cancels it.
Screen mode captures one output. Window mode captures the window's client
image, excluding compositor decorations and the pointer. The pointer option
applies to Screen and Region.

The **View** menu provides fit (Ctrl+0) and zoom (Ctrl++/Ctrl+−); middle-button
dragging pans the preview. Choose tools from **Annotate** to draw arrows, lines,
rectangles, ellipses, freehand strokes, highlights and opaque redactions. Type
text in **Annotate → Annotation properties** (Ctrl+P) and drag a box to place
it, or use Number for successive numbered boxes. Choosing Text opens those
properties automatically. The properties dialogue also sets colour, stroke
width and text size. Press Done or Escape to return to the canvas.
Text uses the bundled Inter font and shares the preview/export painter. Select
an object to move it or delete it. Crop changes the export rectangle without
discarding source pixels. Undo and redo operate on complete gestures. Source
pixels remain immutable; preview, PNG export and image clipboard use the same
painter.

**File → Save As** (Ctrl+S) writes a new PNG atomically and refuses an existing destination,
including a symlink. Choose a fresh filename rather than overwriting another
image. Closing the app, opening an image or taking another screenshot prompts
when annotations have not been saved. PNG export flattens the annotations;
editable objects are held in memory for this initial version.

**Edit** contains undo (Ctrl+Z), redo (Ctrl+Shift+Z or Ctrl+Y), copy image
(Ctrl+C), delete selected annotation (Delete), and reset crop. **Help →
Keyboard shortcuts** (F1) lists the bindings. Actions that cannot run in the
current document or job state are disabled; a dialogue blocks background
commands until it is closed.

`cap [IMAGE]` opens the window. A second invocation activates the existing
instance. `cap --headless` starts a Bus-only service. `--service NAME`,
`--comp NAME` and `--noded-url URL` select the native endpoints. `--version`
prints the exact build provenance before configuration or display access.

Capture originals live in `$MIXOS_APP_HOME/captures`, otherwise
`$MIXOS_APPS_HOME/cap/captures`, then `$MIXOS_VAR/apps/cap/captures`, then
`$XDG_STATE_HOME/mixos/apps/cap/captures`
or `$HOME/.local/state/mixos/apps/cap/captures`.

## Agent commands (cap.v1)

Requests are strict JSON objects. Unknown fields, malformed geometry and
commands that would discard dirty annotations are refused. Status and capture
completion are event driven; callers can read `cap.info` during a job.

| Verb | Request | Result |
|---|---|---|
| `cap.ping`, `cap.info` | `{}` | version, busy state, document and capture metadata |
| `cap.capture` | `{"mode":"screen", "output":"DP-1", "cursor":true, "delay":0}` | completed capture and document |
| `cap.capture` | `{"mode":"window", "window":{"id":7,"generation":3}}` | fenced window capture |
| `cap.capture` | `{"mode":"region", "output":"DP-1"}` | selection, then completed capture |
| `cap.cancel` | `{}` | cancellation requested; region selection also accepts Escape |
| `cap.open` | `{"path":"/home/user/Pictures/image.png"}` | decoded document |
| `cap.annotate` | `{"kind":"arrow","points":[{"x":10,"y":10},{"x":80,"y":60}],"colour":[255,0,0,255],"width":4}` | stable object ID |
| `cap.move` | `{"id":1,"dx":10,"dy":5}` | updated document |
| `cap.delete` | `{"id":1}` | updated document |
| `cap.crop` | `{"crop":{"x":0,"y":0,"width":640,"height":480}}` or `{"crop":null}` | updated document |
| `cap.undo`, `cap.redo` | `{}` | updated document |
| `cap.export` | `{"path":"/home/user/Pictures/new-image.png"}` | saved PNG and document |
| `cap.show` | `{}` | activates GUI; refused by a headless instance |
| `cap.quit` | `{}` | quits if idle and annotations are saved |

Coordinates refer to original image pixels, even after cropping or zooming.
Images are limited to 32 million pixels, documents to 256 objects and 131,072
points, and undo to 128 snapshots within a 16 MiB history budget. A freehand
stroke has at most 16,384 points. Capture requests
complete once; compd applies a three-second capture deadline. Region selection
has its own bounded deadline. A remote cancel during selection is observed
after that native selection finishes; Escape cancels immediately.

Editable document persistence, recording, OCR and
sharing are subsequent work. The current release does not combine outputs into
one desktop image.

## Compositor capture additions

`comp.capture.frame` retains its existing output/window, path and format
behaviour. It additionally accepts `cursor` (default true), `region` in
output-local logical coordinates and an optional `output_generation` fence
for a named output. Region rounding expands to physical pixel boundaries at
fractional scale. Window and region selectors are mutually exclusive.
The generation returned by `comp.region.select` is checked at admission and
again before the image is written. Cursorless capture uses the existing
cursorless render path, including when the native VT is inactive.
Region capture currently refuses rotated or flipped outputs explicitly.
