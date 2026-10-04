use crate::debug::instance::record as record;

/// Start the drain thread. Call exactly once.
pub fn start(rx: crossbeam_channel::Receiver<record::Record>) {
    let _ = std::thread::Builder::new()
        .name("compd-log-drain".into())
        .spawn(move || crate::log::process::instance::drain::drain(rx));
}
