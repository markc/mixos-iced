# Changelog

## 0.1.5

- Restore hover and pressed feedback on compositor-owned iced controls by
  delivering the frame event to rebuilt views before painting. Pointer
  damage stays scoped to its receiving surface and stops when input stops;
  frame-generated messages and scheduled animation wakes are retained.

## 0.1.4

- Reuse toolkit's `CenteredButton` and centred-content helper for compact
  scene targets. Workspace labels retain their corrected placement and hit areas.

## 0.1.3

- Centre panel workspace labels inside their full hit targets. The iced
  scene renderer positions intrinsic content in a bounded wrapper instead
  of compressing centring spacers to zero inside minimum-sized rows.

## 0.1.2

- Extend `comp.capture.frame` with cursor inclusion, output-local logical
  regions and output-generation fences. Existing defaults remain compatible.
- Cursorless file captures share the existing screencopy render paths on
  native KMS, nested and inactive-VT renderers.

## 0.1.1

- Native frame and fenced-window capture with bounded completion replies.
