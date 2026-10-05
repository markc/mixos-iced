// SPDX-License-Identifier: MIT OR Apache-2.0
//! Eager raster decoding with an on-disk change check: [`Images`], a small
//! cache for apps that draw file-backed icons and pictures with iced.
//!
//! A file-backed raster `Handle` loads asynchronously in iced, and a
//! static surface (a compositor page, a panel) can lose the first-frame
//! redraw to another surface consuming the shared notifier, leaving a hole
//! where the icon should be. Decoding here — eagerly, on the caller's
//! thread or its worker — produces an RGBA `Handle` whose pixels and
//! dimensions exist before the first layout, so the first frame already
//! draws them.
//!
//! The cache is keyed by path and validated against the file's
//! modification time and length, so an image edited on disk is re-read
//! while an unchanged one reuses its handle (and its id, keeping GPU
//! uploads stable). Under pressure it discards only entries whose pixel
//! buffers are not shared with a live handle, so nothing on screen loses
//! its backing while the caller holds the `Handle`.
//!
//! SVGs are out of scope: they rasterise per size and belong to an icon
//! resolver (see [`crate::icons`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use iced_core::image::Handle;

/// Entries above this count are pruned (live handles are kept regardless,
/// so the effective size can exceed it while callers hold handles).
const CAP: usize = 512;

type Entry = (Option<SystemTime>, u64, Handle);

/// An eager raster cache. Owned by the app (one per process is usual);
/// `&self` is enough, so it can sit behind an `Arc` and serve any thread.
///
/// ```no_run
/// use toolkit::images::Images;
///
/// let images = Images::new();
/// if let Some(handle) = images.raster("/usr/share/icons/hicolor/48x48/apps/term.png".as_ref()) {
///     // An RGBA handle: pixels and dimensions exist now, not after a redraw.
/// }
/// ```
#[derive(Default)]
pub struct Images {
    cache: Mutex<BTreeMap<PathBuf, Entry>>,
}

impl Images {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode `path` to an RGBA [`Handle`], reusing the cached handle when
    /// the file is unchanged. Returns `None` when the file is missing,
    /// unreadable or not a decodable raster.
    pub fn raster(&self, path: &Path) -> Option<Handle> {
        let metadata = path.metadata().ok()?;
        let stamp = (metadata.modified().ok(), metadata.len());
        // Keep lookup, decoding and insertion atomic: concurrent misses must
        // not create different ids for the same unchanged file.
        let mut cache = self.cache.lock().ok()?;
        if let Some((_, _, handle)) = cache.get(path).filter(|(modified, len, _)| (*modified, *len) == stamp) {
            return Some(handle.clone());
        }
        let pixels = iced_graphics::image::load(&Handle::from_path(path)).ok()?;
        let handle = Handle::from_rgba(pixels.width(), pixels.height(), pixels.into_raw());
        if cache.len() >= CAP {
            // A live handle shares its pixel buffer with the cache. Only
            // discard unused entries, allowing the count to grow while
            // callers still hold icons.
            cache.retain(|_, (_, _, handle)| {
                matches!(handle, Handle::Rgba { pixels, .. } if !pixels.is_unique())
            });
        }
        cache.insert(path.to_owned(), (stamp.0, stamp.1, handle.clone()));
        Some(handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/icon.png")
    }

    #[test]
    fn rasters_are_ready_in_the_first_frame_and_reuse_their_handle() {
        let images = Images::new();
        let first = images.raster(&fixture()).unwrap();
        let second = images.raster(&fixture()).unwrap();
        assert_eq!(first.id(), second.id());
        let Handle::Rgba { width, height, pixels, .. } = first else {
            panic!("a file handle would defer loading");
        };
        assert_eq!((width, height, pixels.len()), (1, 1, 4));
        assert!(pixels[3] > 0);
        assert!(images.raster(Path::new("/missing/icon.png")).is_none());
    }

    #[test]
    fn pressure_preserves_live_handles_and_prunes_unused_entries() {
        // A distinct cache key for the same fixture; no temporary files needed.
        let other = fixture().parent().unwrap().join("../fixtures/icon.png");
        for keep_live in [false, true] {
            let images = Images::new();
            let first = images.raster(&fixture()).unwrap();
            let mut live = Vec::new();
            {
                let mut entries = images.cache.lock().unwrap();
                for index in 0..CAP - 1 {
                    let handle = Handle::from_rgba(1, 1, vec![0; 4]);
                    if keep_live {
                        live.push(handle.clone());
                    }
                    entries.insert(PathBuf::from(format!("unused-{index}.png")), (None, 0, handle));
                }
                assert_eq!(entries.len(), CAP);
            }
            let _other = images.raster(&other).unwrap();
            let second = images.raster(&fixture()).unwrap();
            assert_eq!(first.id(), second.id(), "keep_live={keep_live}");
            assert_eq!(images.cache.lock().unwrap().len(), if keep_live { CAP + 2 } else { 3 });
            drop(live);
        }
    }

    #[test]
    fn concurrent_raster_lookups_share_one_handle() {
        let images = std::sync::Arc::new(Images::new());
        let start = std::sync::Barrier::new(8);
        let handles = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    let images = images.clone();
                    scope.spawn(move || {
                        start.wait();
                        images.raster(&fixture()).unwrap()
                    })
                })
                .collect();
            threads.into_iter().map(|thread| thread.join().unwrap()).collect::<Vec<_>>()
        });
        assert!(handles.iter().all(|handle| handle.id() == handles[0].id()));
        assert_eq!(images.cache.lock().unwrap().len(), 1);
    }
}
