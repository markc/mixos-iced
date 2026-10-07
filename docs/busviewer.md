# BusViewer

BusViewer browses native ABP Bus services, their advertised verbs and mesh membership. It is a Wayland app using MixOS's shared application host, toolkit and appearance, with the desktop's selected font and icon weights.

Run `busviewer` or choose BusViewer in the launcher. A second launch restores and focuses the existing window. Use `--noded-url URL`, `--service NAME` or `--comp NAME` to select an isolated session; normal launches use the shared node configuration.

## Browse and call

Expand a service in the left tree, then select a verb. Search matches service names, verb names, arguments and descriptions. Details show the argument signature, description and advertised read-only flag. Legacy descriptions keep safety as **unknown**.

Enter an optional JSON body in the multiline editor. Choose **Bus → Call verb** or press **Ctrl+Enter**. Invalid JSON is refused before sending. An empty body is sent empty, preserving the original BusViewer contract. Only one call runs at a time. Its original service, verb and request body remain attached to its reply even if the selection changes.

Replies show the actual return code and body. JSON is formatted; plain-text errors remain readable. A transport failure reports an unknown outcome and **never retries the call**. The request body is limited to 65,536 bytes; the displayed reply is limited to 1,000,000 bytes. The underlying Bus framing limits still apply.

**Edit** provides JSON formatting, body clearing and reply copying. **File → Refresh services** performs discovery again. Registrations and reconnects also refresh through native Bus events, without a poller. Failed descriptions remain visible under their services; select an error row to read both probe failures in the details pane. One failed probe does not stop the others, and a failed broker lookup preserves the last known selection. The Mesh nodes branch shows membership, as in the original app; remote service browsing is a later extension. Membership is refetched with discovery. The current noded profile has no membership-change topic, so an authority-only change requires Refresh.

## Keyboard

| Shortcut | Action |
|---|---|
| Ctrl+R | Refresh services |
| Ctrl+Enter | Call selected verb |
| Ctrl+Shift+F | Format JSON body |
| Ctrl+Q | Quit after accepted work completes |
| F1 | Keyboard shortcuts |
| Alt+F / E / B / H, F10 | Open menus |
| Escape | Close dialogue |

## Native app contract

`HELP` and `app.describe` expose the contract. `busviewer.ping` probes identity; `busviewer.info` returns discovery, selected verb, body, last reply and UI state. `busviewer.show` restores and focuses the existing window through compd with its PID and generation fenced identity.

`busviewer.refresh` returns after discovery completes. `busviewer.select` takes both `service` and `verb`, which must exist in the current discovery. `busviewer.call` accepts both target fields or neither, plus optional `body` containing JSON text. Omitting the target uses the current selection; partial or invalid targets never fall back silently. Omitting the body uses the editor text.

The call envelope retains the target's `rc` and raw `body`. Envelope rc 0 means the transport received a response, including target refusal rc 10 or greater. Envelope rc 10 reports invalid input, busy state or a transport error. Automated callers must inspect both levels. Mutating app commands refuse while a dialogue or operation blocks them. `busviewer.quit` refuses while busy; closing the window waits for accepted work.

## Port provenance and verification

Discovery parsers and behaviour were adapted from Cosmix `70e6c9233a02577099e9c14500aebf44856530e1`, `src/desktop/apps/busviewer`. Bevy ECS and CTK rendering were replaced by plain state, iced messages/tasks/subscriptions, and shared tree, split, menu and modal controls. No D-Bus client, portal or alternate transport is required.

Build and run unit/UI tests with `cargo test --locked -p busviewer`. Native broker acceptance is separate, on a kernel supporting the native session admission path. See [Bevy to iced](dev/bevy-to-iced.md) for the reusable port method.
