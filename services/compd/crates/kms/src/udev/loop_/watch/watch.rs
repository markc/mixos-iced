//! The long-lived udev watcher as a loop source. Previously the UdevBackend
//! was created for a single device-path lookup and dropped; it is now
//! retained so Added/Changed/Removed events flow (the hotplug detection
//! path). Registration into the event loop lives in
//! `native.wire/wire.plugin` (Law 4).
//! Failure policy: if the snapshot worked moments earlier, a failing watch is
//! not self-recovering — panic.

pub use smithay::backend::udev::UdevBackend as UdevWatch;

pub fn watch(seat: &str) -> UdevWatch {
    UdevWatch::new(seat).expect("udev watch creation failed")
}
