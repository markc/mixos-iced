# Changelog

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
