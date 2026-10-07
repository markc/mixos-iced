# Local queue retirement patch

Base: crates.io wayland-client 0.31.15, archive SHA-256
`e3c36a0f861ad76d0901f2800b46321410d9f73f2ea88aac0650d86c32688073`.
The archive records wayland-rs commit
`05196740da57e41b60a9e9f8e35079e9bd29b89a` (see `.cargo_vcs_info.json`
for the authoritative full identifier).

`src/event_queue.rs` closes EventQueue on drop and takes its queued events
under the queue mutex, then retires userdata and wakers outside the mutex.
Subsequent native events preserve child-data creation but cannot enter the
closed queue. Rejected userdata is also dropped after unlocking, including the
registry-forwarding owner in `src/globals.rs`.

Without this, an undrained queue retains QueueProxyData, which retains its
QueueHandle and therefore the queue itself. Presentation request credits and
other owned callback resources then survive queue retirement indefinitely.
The fix adds no wire message, worker, queue or alternate connection.

Guards cover queued callback-data retirement and deliveries to a closed queue.
The native desktop probe additionally checks real callback and presentation
userdata without typed event dispatch. These layers must be reported
separately; queue structure tests alone do not prove a compositor outcome.
