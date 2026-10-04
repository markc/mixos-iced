use slots::channel::router::base::ChannelRouter;
use slots::launch::types::types::LaunchOutcome;

/// The launch-completed event channel. Declared here so there is a single
/// sender; listeners (world systems) import the public `EXECUTED` token.
pub mod event {
    use slots::launch::types::types::LaunchOutcome;
    slots::define_channel!(pub EXECUTED, EXECUTED_TX: LaunchOutcome);
}

/// Transparently broadcast a completed launch onto a world's channel router.
/// The loader's outcome receiver passes the focused world's router here (for
/// inline and off-thread launches alike), so every outcome reaches listeners
/// through one path. Mirrors the rim `announce_*` senders — takes the router
/// rather than the whole `Loop` so this crate stays below `orchestration.core`
/// (the placeholder system, which listens, is upstream of it).
pub fn broadcast(channels: &mut ChannelRouter, outcome: LaunchOutcome) {
    channels.send(&event::EXECUTED_TX, outcome);
}
