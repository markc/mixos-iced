//! DrmDevice (+ notifier) from an externally-opened fd. How the fd was opened
//! (libseat) is not this crate's business (Law 1).
//! Failure policy: the selected device must open — panic (original unwrap).

use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmDeviceNotifier};
use smithay::utils::DeviceFd;
use std::os::unix::io::OwnedFd;

/// Wrap a raw owned fd (from `seat.interface/interface.open`) as a DrmDeviceFd.
pub fn wrap_fd(fd: OwnedFd) -> DrmDeviceFd {
    DrmDeviceFd::new(DeviceFd::from(fd))
}

/// Create the DrmDevice and its event notifier. `disable_connectors` mirrors
/// the original `DrmDevice::new(fd, true)`.
pub fn open(fd: DrmDeviceFd) -> (DrmDevice, DrmDeviceNotifier) {
    DrmDevice::new(fd, true).expect("DrmDevice creation failed")
}
