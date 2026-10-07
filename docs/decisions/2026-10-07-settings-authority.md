# Desktop settings have one native ABP authority

Status: accepted architecture, implementation staged. Contract version 0.1.0.

Desktop appearance and layout belong to a headless `settingsd` authority. Settings
controls, Mix operators and agents submit validated batches through native ABP.
Applications and compd/Quoin consume complete accepted snapshots through shared
headless `settings` contracts and renderer adapters in `appearance`. Closing a
Settings page does not stop delivery. Other services retain their own hardware
and operational settings; discovery does not create a distributed transaction.

The shared library has multiple external owners from its first commit:
settingsd, applications and compd. The plain component names `settings` and
`settingsd` describe their public contracts. Config remains independent of Bus;
the toolkit remains independent of settings transport. No COSMIC implementation
or GPL/MPL code is incorporated.

Settings behaviour has one shared implementation per concern: `settings`
consumer ordering/recovery and render-domain comparison; `appearance`
projection-to-toolkit/iced mapping; `toolkit` reusable controls and role/default
builders; and `application` reusable event-loop activation/invalidation.
Applications retain their service lifetime, content and deliberate overrides.
Compd owns output geometry, work areas, Wayland configuration and presentation.
Standard widgets inherit current defaults without copied app palettes, font
captures or settings handlers. Extract repeated components during migrations;
keep headless crates independent of renderers and toolkit independent of Bus.

Each profile has one exclusive writer. Authored settings, revision and bounded
operation receipts share a durable strict-data replacement. A no-op persists a
receipt without changing semantic revision. The durable store incarnation fences
restores; broker sequence is delivery order, not settings revision. Caller access
remains open across the trusted mesh. Correct target/revision binding and
canonical publisher ownership do not require the operator to own the session.

Native calls/events use existing ABP/noded, preserving frozen framing and
identity constants. External toolkit D-Bus adapters are later compatibility
boundaries inside the independent MixOS session. Retained broker snapshots are
presentation/recovery aids, not a durable offline store. Recovery is driven by
lifecycle/connection/loss events, without an idle settings polling loop.

Consumer application and frame presentation are separate evidence. Compositor
geometry/input updates remain coherent while Wayland configure/ack/buffer changes
complete asynchronously. No global same-vblank guarantee is made.

The initial implementation is an isolated headless authority/store slice.
Renderer wiring, named-profile management, previews, replication and compatibility
remain later work. This decision establishes ownership; it does not claim those
runtime capabilities are already shipped. See the [initial contract](../spec/settings/README.md).
