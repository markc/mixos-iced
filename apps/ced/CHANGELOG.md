# Ced Bus contract

## 0.1.6

Use the shared application host for native bootstrap, window configuration and
the single task worker. The retained Bus contract and deferred close behaviour
remain unchanged.
Render and handle editor input through toolkit's `EditorPane`, with a borrowed
adapter to the existing buffer, highlighting and OT engine. Commands and Bus
state continue through the established controller.

## 0.1.5

Transplant the existing `ced.*` verbs and retained golden fixtures from
cosmix `e0297242305f3a3c3de09f1ca01e8faa771768da`. Adopt the single workspace
iced snapshot, native ABP client and isolated MixOS state paths.
