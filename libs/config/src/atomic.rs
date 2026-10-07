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
    directory(path, false)
}

/// Provision an absolute directory through held parent descriptors. Missing
/// components are private (0700); existing permissions are left unchanged.
/// Validate the entire path before creating anything and never follow links.
pub fn create_directory(path: &Path) -> io::Result<File> {
    directory(path, true)
}

fn directory(path: &Path, create: bool) -> io::Result<File> {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    if !path.is_absolute() {
        return Err(io::Error::other("expected an absolute directory"));
    }
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => names.push(std::ffi::CString::new(name.as_bytes())?),
            _ => return Err(io::Error::other("parent/prefix traversal refused")),
        }
    }
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open("/")?;
    for name in names {
        let mut fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 && create && io::Error::last_os_error().kind() == io::ErrorKind::NotFound {
            if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) } != 0
                && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists
            {
                return Err(io::Error::last_os_error());
            }
            fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        }
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(directory)
}

/// Bounded read of one regular file in a held directory. O_NONBLOCK prevents
/// opening a FIFO from hanging before the regular-file metadata check.
pub fn read_in(dir: &File, name: &std::ffi::OsStr, limit: usize) -> io::Result<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    if name.as_bytes().is_empty() || name.as_bytes().contains(&b'/') || name == "." || name == ".."
    {
        return Err(io::Error::other("expected a single file name"));
    }
    let name = std::ffi::CString::new(name.as_bytes())?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(io::Error::other("expected a bounded regular file"));
    }
    let mut bytes = Vec::new();
    file.take(
        (limit as u64)
            .checked_add(1)
            .ok_or_else(|| io::Error::other("invalid byte limit"))?,
    )
    .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::other("file grew beyond byte limit"));
    }
    Ok(bytes)
}

/// Open a nested regular file under a held directory descriptor, walking
/// every intermediate component with `openat` and refusing a symlink at
/// any level, including the final file. The returned descriptor is bound
/// to the opened inode, not to the walked path: renaming or replacing any
/// component afterwards changes what a path-based open finds, never what
/// this descriptor reads.
///
/// `relative` must be a relative path of plain components: no leading
/// `/`, no `.`, `..`, empty or NUL-bearing components. The raw bytes are
/// validated before any open, so `Path` normalisation cannot smuggle an
/// interior `.`, a repeated `/` or a trailing `/` past the check;
/// non-UTF-8 plain names are accepted. O_NONBLOCK on the final open
/// prevents a FIFO from hanging before the regular-file metadata check,
/// and the opened inode is verified to be a regular file before it is
/// returned.
pub fn open_nested(dir: &File, relative: &Path) -> io::Result<File> {
    use std::os::unix::ffi::OsStrExt;
    let raw = relative.as_os_str().as_bytes();
    if raw.is_empty() || raw[0] == b'/' {
        return Err(io::Error::other(
            "expected a relative path of plain components",
        ));
    }
    let mut names: Vec<&[u8]> = Vec::new();
    for part in raw.split(|byte| *byte == b'/') {
        if part.is_empty() || part == b"." || part == b".." {
            return Err(io::Error::other(
                "expected a relative path of plain components",
            ));
        }
        if part.contains(&0) {
            return Err(io::Error::other("component contains a NUL byte"));
        }
        names.push(part);
    }
    let (file_name, parents) = names
        .split_last()
        .ok_or_else(|| io::Error::other("expected a file name"))?;
    let component = |bytes: &[u8]| {
        std::ffi::CString::new(bytes).map_err(|_| io::Error::other("component contains a NUL byte"))
    };
    let mut current = dir.try_clone()?;
    for name in parents {
        let name = component(name)?;
        let fd = unsafe {
            libc::openat(
                current.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        current = unsafe { File::from_raw_fd(fd) };
    }
    let name = component(file_name)?;
    let fd = unsafe {
        libc::openat(
            current.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("expected a regular file"));
    }
    Ok(file)
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
    fn provisioning_rejects_links_and_traversal_before_external_side_effects() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("settings-provision-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let outside = root.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        assert!(create_directory(&root.join("link/escaped")).is_err());
        assert!(!outside.join("escaped").exists());
        assert!(create_directory(&root.join("uncreated/../escape")).is_err());
        assert!(!root.join("uncreated").exists());
        assert!(create_directory(Path::new("relative/cache")).is_err());
        let target = root.join("owned/cache/settings");
        let held = create_directory(&target).unwrap();
        assert_eq!(held.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        assert!(create_directory(&target).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

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

    #[test]
    fn open_nested_walks_and_the_descriptor_survives_a_rename() {
        use std::io::Read;
        let root = std::env::temp_dir().join(format!("settings-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/file.txt"), b"pinned").unwrap();
        let held = open_directory(&root).unwrap();
        let mut file = open_nested(&held, Path::new("a/b/file.txt")).unwrap();
        // The path is replaced after the descriptor open: the held
        // descriptors still read the pinned bytes, a path-based open
        // through the same root finds the replacement.
        std::fs::rename(root.join("a"), root.join("old-a")).unwrap();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/file.txt"), b"replacement").unwrap();
        let mut bytes = String::new();
        file.read_to_string(&mut bytes).unwrap();
        assert_eq!(bytes, "pinned");
        assert!(open_nested(&held, Path::new("a/b/file.txt")).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn open_nested_refuses_symlinks_at_every_level() {
        let root =
            std::env::temp_dir().join(format!("settings-nested-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("outside")).unwrap();
        std::fs::write(root.join("a/file.txt"), b"pinned").unwrap();
        std::fs::write(root.join("outside/evil.txt"), b"outside").unwrap();
        let held = open_directory(&root).unwrap();
        // A symlinked intermediate directory is refused, so the outside
        // file is never read.
        std::os::unix::fs::symlink(root.join("outside"), root.join("a/link")).unwrap();
        assert!(open_nested(&held, Path::new("a/link/evil.txt")).is_err());
        std::fs::remove_file(root.join("a/link")).unwrap();
        // A symlinked final component is refused.
        std::fs::remove_file(root.join("a/file.txt")).unwrap();
        std::os::unix::fs::symlink(root.join("outside/evil.txt"), root.join("a/file.txt")).unwrap();
        assert!(open_nested(&held, Path::new("a/file.txt")).is_err());
        std::fs::remove_file(root.join("a/file.txt")).unwrap();
        // The whole directory component swapped for a symlink escape.
        std::fs::remove_dir(root.join("a")).unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join("a")).unwrap();
        assert!(open_nested(&held, Path::new("a/evil.txt")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn open_nested_refuses_escapes_and_non_regular_components() {
        use std::os::unix::ffi::OsStrExt;
        let root =
            std::env::temp_dir().join(format!("settings-nested-refuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a/file.txt"), b"data").unwrap();
        let held = open_directory(&root).unwrap();
        assert!(open_nested(&held, Path::new("a/../file.txt")).is_err());
        assert!(open_nested(&held, Path::new("/absolute")).is_err());
        assert!(open_nested(&held, Path::new("")).is_err());
        assert!(open_nested(&held, Path::new("a")).is_err()); // a directory
        // A FIFO would block a naive open forever; O_NONBLOCK plus the
        // regular-file check refuses it instead.
        let fifo = root.join("a/pipe");
        let fifo = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(open_nested(&held, Path::new("a/pipe")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn open_nested_refuses_normalised_components_and_nul_bytes() {
        use std::ffi::OsStr;
        use std::io::Read;
        use std::os::unix::ffi::OsStrExt;
        let root = std::env::temp_dir().join(format!("settings-nested-raw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a/file.txt"), b"data").unwrap();
        let held = open_directory(&root).unwrap();
        // `Path` would silently normalise these; the raw-bytes walk
        // refuses each one before any open.
        assert!(open_nested(&held, Path::new("a/./file.txt")).is_err());
        assert!(open_nested(&held, Path::new("a//file.txt")).is_err());
        assert!(open_nested(&held, Path::new("a/file.txt/")).is_err());
        assert!(open_nested(&held, Path::new(OsStr::from_bytes(b"a/fi\0le.txt"))).is_err());
        // A plain non-UTF-8 component still walks and reads.
        std::fs::write(
            root.join("a").join(OsStr::from_bytes(b"fi\xffle.txt")),
            b"plain",
        )
        .unwrap();
        let nested = OsStr::from_bytes(b"a/fi\xffle.txt");
        let mut file = open_nested(&held, Path::new(nested)).unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"plain");
        std::fs::remove_dir_all(root).unwrap();
    }
}
