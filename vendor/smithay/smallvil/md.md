# Fix

### Reasoning on the grabs/handlers code and removing un-necessary code
The grabs, handlers are unknown.

### Moving into crates and encapsulation

Adjust current structure to work in crates
Encapsulate logic into crates
Be wary of circular crate dependency. implement trait to fix transparent types. ( defining only whats 'required' )
The main state must use implementation traits or whatever to make it basic and expanded.

### Decoupling from winit and renderer specific (GLES)

Make sure that the renderer that uses GLES is generalized.

### DRM SUpport

Add logout button

Add deploy to DRM script:

- Build on container
- Add to the DRM login thing(GDM)

# There should be no header for the window.

When hovering a window at its top side, show a small topbar, with the window name
Otherwise, do not render a window header and just keep a nice border.

# New Windows

New windows should be open at viewport center at consistent size

# Selection

- Allow selection of windows. A window become 'selected' when the top bar is clicked once, and deselected when its
  clicked again. this doesnt change the window focus.
- When shift is held, and another window is clicked on the topbar, the selection is added. otherwise when shift not
  clicked, the selection is shifted and focus is reset to the new window.
- Any click outside a topbar should cancel selection
- For multiple selection, the last clicked window is the primary window. selection actions are biased toward the primary
  selected.

# Selection action:

align, distribute and stretch functions.
Fit on screen function

# Action: layout and alignments

Set zoom level to 1
Set zoom level to 1 and scale focused window to match
Temporarily set the window as sticky to full screen and always activated on zoom level one.
Stretch window on screen
Layout multiple windows on screen

# Action: Launcher

1. Open a window to the right of the current window at the same size. ( with the icon based process finder )
   If a window currently reside on the right, attempt to fix collision by moving every window on the right until there
   are no collisions.
2. Open a window to the right attempting a layout tile(4x4 grids were never used, so only for rows)

The same functions for movement, moving exactly the dimension of the window

# Action: Navigation

1. Find the next window toward direction ( moving a full screen up/down ) preferring the one on the top left.
   Smoothly pan into it keeping current zoom levels, such that it is on the top left.
2. Move exactly monitor size toward the direction.
3. Reset navigation to first available window

# Safety feature:

Change border of window to red if its overlapping another window.

# Overview

As a way to see all windows in a sequential scrollable grid at consistent size, clicking one exists the overview and
navigates to it.
(Also, search and move by title/process)

# Keeping window size preference by specificity.

Process ID ->
Process Arguments -> Title
Title

# Not normal windows

Something else, like intellij overlays, etc. these are 'special' windows that like to position themselves.
(Popups may not be TopLevelElement, while dialogs may just be determined as windows with parents)
let is_subwindow = window.toplevel().unwrap().parent().is_some();

# Preserved State
- Add layers, preserved state for collection of windows
- Without explicitly closing a window, the window placeholder will remain and have a button to relaunch it in the previous dimensions.
- With layers, add ability to have them just as the workspace ( sequential layers, move up and down to different canvases. )

# Automatic Alignments

- Add snap to a small grid based ( base on zoom level )
- Add snap to other windows and rulers
- Add rulers in the layer


# Control Groups
Window not visible on screen will not receive input events even when focused.
When the window is not visible, hitting space should focus it.

Control group: Multiple window selection + Shift + F1,2,3 to assign a group, then F1,F2,F3 moves camera to focus on the group.

When a single window is set as group, then it should just move to it and full screen it.

Add the support for per-"actor"(render element) shader. ( which should support displacement, color filters )
