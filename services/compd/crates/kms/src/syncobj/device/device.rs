//! Kernel syncobj mechanics on a device: create/import/destroy + the
//! eventfd-support probe the wire previously called inline.
//! Protocol policy (linux-drm-syncobj-v1) stays compositor-side and consumes
//! these as plain values (Law 1/3).

use smithay::backend::drm::DrmDeviceFd;
use smithay::reexports::drm::control::{syncobj, Device as ControlDevice};
use std::os::unix::io::{BorrowedFd, OwnedFd};

/// Whether the device supports DRM_IOCTL_SYNCOBJ_EVENTFD — the gate the
/// syncobj protocol global checks before advertising.
pub fn supports_eventfd(device: &DrmDeviceFd) -> bool {
    smithay::wayland::drm_syncobj::supports_syncobj_eventfd(device)
}

pub fn create(device: &DrmDeviceFd, signalled: bool) -> std::io::Result<syncobj::Handle> {
    device.create_syncobj(signalled)
}

pub fn destroy(device: &DrmDeviceFd, handle: syncobj::Handle) -> std::io::Result<()> {
    device.destroy_syncobj(handle)
}

/// Import a syncobj from an fd (`import_sync_file = false`: the fd IS a
/// syncobj fd, not a sync_file).
pub fn import(device: &DrmDeviceFd, fd: BorrowedFd<'_>) -> std::io::Result<syncobj::Handle> {
    device.fd_to_syncobj(fd, false)
}

/// Export a syncobj as an fd (`export_sync_file = false`).
pub fn export(device: &DrmDeviceFd, handle: syncobj::Handle) -> std::io::Result<OwnedFd> {
    device.syncobj_to_fd(handle, false)
}
