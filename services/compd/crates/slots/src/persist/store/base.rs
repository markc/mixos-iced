use crate::persist::envelope::base as envelope;
use crate::persist::write::base::atomic_write;
use std::io;
use std::path::{Path, PathBuf};

/// A table directory `<state>/<table>`. Records live at `id/{id}/record.json` (an
/// `Envelope`); partitions are symlinks at `{key}/{value}/{id}` -> `../../id/{id}`.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(table_dir: PathBuf) -> Self {
        Self { dir: table_dir }
    }

    fn record_dir(&self, id: &str) -> PathBuf {
        self.dir.join("id").join(id)
    }
    fn part_link(&self, key: &str, value: &str, id: &str) -> PathBuf {
        self.dir.join(key).join(value).join(id)
    }

    /// Write (or overwrite) a record atomically and refresh its partition symlinks.
    pub fn put(&self, id: &str, key: &str, version: u32, data: &[u8], partitions: &[(&str, &str)]) -> io::Result<()> {
        let bytes = envelope::wrap(key, version, data).map_err(io::Error::other)?;
        atomic_write(&self.record_dir(id).join("record.json"), &bytes)?;
        for (pk, pv) in partitions {
            let link = self.part_link(pk, pv, id);
            if let Some(parent) = link.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(Path::new("../../id").join(id), &link)?;
        }
        Ok(())
    }

    /// Remove a record folder and its partition symlinks.
    pub fn delete(&self, id: &str, partitions: &[(&str, &str)]) -> io::Result<()> {
        ignore_missing(std::fs::remove_dir_all(self.record_dir(id)))?;
        self.unlink_partitions(id, partitions)
    }

    /// Remove just a record's partition symlinks (e.g. when it moves partition),
    /// leaving the primary record in place.
    pub fn unlink_partitions(&self, id: &str, partitions: &[(&str, &str)]) -> io::Result<()> {
        for (pk, pv) in partitions {
            ignore_missing(std::fs::remove_file(self.part_link(pk, pv, id)))?;
        }
        Ok(())
    }

    /// Read a record's `(version, data_bytes)`, or `None` if absent.
    pub fn get(&self, id: &str) -> io::Result<Option<(u32, Vec<u8>)>> {
        match std::fs::read(self.record_dir(id).join("record.json")) {
            Ok(b) => envelope::unwrap(&b).map(Some).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// All record ids in the primary index.
    pub fn list(&self) -> io::Result<Vec<String>> {
        entries(&self.dir.join("id"))
    }

    /// All record ids in a partition (the symlink names).
    pub fn list_partition(&self, key: &str, value: &str) -> io::Result<Vec<String>> {
        entries(&self.dir.join(key).join(value))
    }
}

fn entries(dir: &Path) -> io::Result<Vec<String>> {
    match std::fs::read_dir(dir) {
        Ok(rd) => Ok(rd
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

fn ignore_missing(r: io::Result<()>) -> io::Result<()> {
    match r {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}
