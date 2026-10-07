// SPDX-License-Identifier: MIT OR Apache-2.0
//! Durable same-directory replacement. Errors after rename are ambiguous:
//! callers must recover/read back rather than report an uncommitted operation.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Open an absolute directory without following a symlink in ANY component.
/// The caller owns the returned inode; relative/parent traversal is refused.
pub fn open_directory(path: &Path) -> io::Result<File> {
    use std::path::Component;
    use std::os::unix::ffi::OsStrExt;
    if !path.is_absolute() { return Err(io::Error::other("expected an absolute directory")); }
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let mut directory = OpenOptions::new().read(true).custom_flags(flags).open("/")?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {},
            Component::Normal(name) => {
                let name = std::ffi::CString::new(name.as_bytes())?;
                let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
                if fd < 0 { return Err(io::Error::last_os_error()); }
                directory = unsafe { File::from_raw_fd(fd) };
            },
            _ => return Err(io::Error::other("parent/prefix traversal refused")),
        }
    }
    Ok(directory)
}

/// Bounded read of one regular file in a held directory. O_NONBLOCK prevents
/// opening a FIFO from hanging before the regular-file metadata check.
pub fn read_in(dir: &File, name: &std::ffi::OsStr, limit: usize) -> io::Result<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    if name.as_bytes().is_empty() || name.as_bytes().contains(&b'/') || name == "." || name == ".." {
        return Err(io::Error::other("expected a single file name"));
    }
    let name = std::ffi::CString::new(name.as_bytes())?;
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    if fd < 0 { return Err(io::Error::last_os_error()); }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit as u64 { return Err(io::Error::other("expected a bounded regular file")); }
    let mut bytes = Vec::new();
    file.take((limit as u64).checked_add(1).ok_or_else(|| io::Error::other("invalid byte limit"))?).read_to_end(&mut bytes)?;
    if bytes.len() > limit { return Err(io::Error::other("file grew beyond byte limit")); }
    Ok(bytes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Prepare,
    FileSync,
    Rename,
    DirectorySync,
}

#[derive(Debug)]
pub struct ReplaceError {
    pub stage: Stage,
    pub may_have_replaced: bool,
    pub source: io::Error,
}

impl std::fmt::Display for ReplaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "atomic replacement {:?}: {}", self.stage, self.source)
    }
}
impl std::error::Error for ReplaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Parent must already exist. Directory-relative operations keep the rename
/// and sync bound to the same opened directory even if its path is renamed.
pub fn replace(path: &Path, bytes: &[u8]) -> Result<(), ReplaceError> {
    replace_with(path, bytes, |_| Ok(()))
}

fn replace_with(
    path: &Path,
    bytes: &[u8],
    before: impl FnMut(Stage) -> io::Result<()>,
) -> Result<(), ReplaceError> {
    let prepare = |source| ReplaceError {
        stage: Stage::Prepare,
        may_have_replaced: false,
        source,
    };
    let parent = path
        .parent()
        .ok_or_else(|| prepare(io::Error::other("missing parent")))?;
    let name = path
        .file_name()
        .ok_or_else(|| prepare(io::Error::other("missing file name")))?;
    let dir = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent)
        .map_err(prepare)?;
    replace_in_with(&dir, name, bytes, before)
}

/// Replace a single file in a caller-held directory. The lock owner can bind
/// reads, replacements and directory syncs to this same inode throughout life.
pub fn replace_in(dir: &File, name: &std::ffi::OsStr, bytes: &[u8]) -> Result<(), ReplaceError> {
    replace_in_with(dir, name, bytes, |_| Ok(()))
}

fn replace_in_with(
    dir: &File,
    name: &std::ffi::OsStr,
    bytes: &[u8],
    mut before: impl FnMut(Stage) -> io::Result<()>,
) -> Result<(), ReplaceError> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let mut stage = Stage::Prepare;
    let mut renamed = false;
    let result = (|| -> io::Result<()> {
        before(stage)?;
        use std::os::unix::ffi::OsStrExt;
        if name.as_bytes().is_empty()
            || name.as_bytes().contains(&b'/')
            || name == "."
            || name == ".."
        {
            return Err(io::Error::other("expected a single file name"));
        }
        let name = std::ffi::CString::new(name.as_bytes())?;
        let temp = std::ffi::CString::new(format!(
            ".replace-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))?;
        // Never dereference an existing destination symlink.
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        let rc = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc == 0 {
            if unsafe { metadata.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFREG {
                return Err(io::Error::other("destination is not a regular file"));
            }
        } else if io::Error::last_os_error().kind() != io::ErrorKind::NotFound {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                temp.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let write = (|| -> io::Result<()> {
            file.write_all(bytes)?;
            stage = Stage::FileSync;
            before(stage)?;
            file.sync_all()?;
            stage = Stage::Rename;
            before(stage)?;
            if unsafe {
                libc::renameat(
                    dir.as_raw_fd(),
                    temp.as_ptr(),
                    dir.as_raw_fd(),
                    name.as_ptr(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            renamed = true;
            stage = Stage::DirectorySync;
            before(stage)?;
            dir.sync_all()
        })();
        if !renamed {
            unsafe {
                libc::unlinkat(dir.as_raw_fd(), temp.as_ptr(), 0);
            }
        }
        write
    })();
    result.map_err(|source| ReplaceError {
        stage,
        may_have_replaced: renamed,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_pre_rename_keeps_old_bytes_post_rename_is_ambiguous() {
        let dir = std::env::temp_dir().join(format!("settings-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("desktop.conf.mix");
        for fail in [
            Stage::Prepare,
            Stage::FileSync,
            Stage::Rename,
            Stage::DirectorySync,
        ] {
            replace(&path, b"old").unwrap();
            let error = replace_with(&path, b"new", |stage| {
                if stage == fail {
                    Err(io::Error::other("injected"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
            assert_eq!(error.may_have_replaced, fail == Stage::DirectorySync);
            assert_eq!(
                std::fs::read(&path).unwrap(),
                if error.may_have_replaced {
                    b"new"
                } else {
                    b"old"
                }
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn symlink_destination_does_not_overwrite_external_file() {
        let dir = std::env::temp_dir().join(format!("settings-symlink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let outside = dir.join("outside");
        std::fs::write(&outside, b"keep").unwrap();
        let path = dir.join("candidate");
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(replace(&path, b"bad").is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
