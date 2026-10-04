//! Quoin's `conf.mix` page order, `panels.{left,bottom,right,top}` (N2b):
//! read when the host starts, rewritten by `shell.panel.order`.
//!
//! There is no file watcher: each write re-reads the file first, so a hand
//! edit is never lost to it, but a hand edit alone is not ingested until the
//! next write or restart. The path is Quoin's: `<etc>/quoin/conf.mix`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use config::{Dir, Value, parse as parse_mix_data};
use edges::Edge as ShellEdge;
use serde_json::json;

/// Each edge's declared page order, indexed by `edges::Edge::index`.
pub type Declared = [Vec<String>; 4];

/// Most pages `shell.panel.order` accepts on one edge (Quoin's).
pub const MAX_ORDER_PAGES: usize = 32;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// The one path every reader and writer uses (Quoin `conf_mix_path`).
pub fn conf_mix_path() -> PathBuf {
    config::path(Dir::Etc).join("quoin/conf.mix")
}

/// `left` / `bottom` / `right` / `top`.
pub fn edge_name(edge: ShellEdge) -> &'static str {
    match edge {
        ShellEdge::Left => "left",
        ShellEdge::Bottom => "bottom",
        ShellEdge::Right => "right",
        ShellEdge::Top => "top",
    }
}

fn parse_edge(name: &str) -> Option<ShellEdge> {
    ShellEdge::ALL.into_iter().find(|edge| edge_name(*edge) == name)
}

/// A sub-panel identifier (Quoin config.rs `identifier`).
fn identifier(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}

/// The declarations in a parsed conf.mix: `panels` absent means none. Any
/// other key is left to Quoin's schema; only `panels` is read here, and its
/// shape is checked as Quoin's `ShellConfig::parse` checks it.
fn declared_of(value: &Value) -> Result<Declared, String> {
    let Value::Map(root) = value else { return Err("conf.mix is not a map".into()) };
    let mut declared = Declared::default();
    let Some(panels) = root.get("panels") else { return Ok(declared) };
    let Value::Map(panels) = panels else { return Err("conf.mix panels is not a map".into()) };
    for (name, pages) in panels.iter() {
        let edge = parse_edge(name).ok_or_else(|| format!("panels.{name} is not an edge"))?;
        let Value::List(pages) = pages else { return Err(format!("panels.{name} is not a list")) };
        declared[edge.index()] = pages
            .iter()
            .map(|page| match page {
                Value::String(page) if identifier(page) => Ok(page.clone()),
                _ => Err(format!("panels.{name}: page names must be non-empty sub-panel identifiers")),
            })
            .collect::<Result<_, _>>()?;
    }
    check_unique(&ShellEdge::ALL.map(|edge| (edge, declared[edge.index()].clone())))
        .map_err(|refusal| refusal.message)?;
    Ok(declared)
}

/// The declarations in `path`; a missing file declares nothing.
pub fn read_declared(path: &Path) -> Result<Declared, String> {
    match std::fs::read_to_string(path) {
        Ok(source) => declared_of(&parse_mix_data(&source).map_err(|error| error.to_string())?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Declared::default()),
        Err(error) => Err(error.to_string()),
    }
}

/// A `shell.panel.order` refusal: rc 10 `{error_code, message, edges?}`.
#[derive(Debug, PartialEq)]
pub struct OrderRefusal {
    pub code: &'static str,
    pub message: String,
    pub edges: Vec<&'static str>,
}

impl OrderRefusal {
    fn invalid(message: String, edges: Vec<&'static str>) -> Self {
        Self { code: "INVALID_ARGUMENT", message, edges }
    }

    fn write(message: String) -> Self {
        Self { code: "CONFIG_WRITE", message, edges: Vec::new() }
    }

    pub fn body(&self) -> serde_json::Value {
        let mut body = json!({"error_code": self.code, "message": self.message});
        if !self.edges.is_empty() {
            body["edges"] = json!(self.edges);
        }
        body
    }
}

/// Parse a `shell.panel.order` body, `{edges:{<edge>:[string], …}}` with one
/// to four edges, every name a sub-panel identifier, at most
/// [`MAX_ORDER_PAGES`] per edge, none repeated within or across the named
/// edges. In `Edge::ALL` order.
pub fn parse_order(body: &serde_json::Value) -> Result<Vec<(ShellEdge, Vec<String>)>, OrderRefusal> {
    let invalid = |message: &str| OrderRefusal::invalid(message.to_owned(), Vec::new());
    let edges = body
        .get("edges")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| invalid("edges must be a map of edge to page list"))?;
    if edges.is_empty() || edges.len() > 4 {
        return Err(invalid("edges must name one to four edges"));
    }
    if let Some(name) = edges.keys().find(|name| parse_edge(name).is_none()) {
        return Err(OrderRefusal::invalid(format!("{name} is not an edge (left, bottom, right or top)"), Vec::new()));
    }
    let mut order = Vec::new();
    for edge in ShellEdge::ALL {
        let name = edge_name(edge);
        let Some(pages) = edges.get(name) else { continue };
        let pages = pages.as_array().ok_or_else(|| invalid("each edge takes a list of page names"))?;
        if pages.len() > MAX_ORDER_PAGES {
            return Err(OrderRefusal::invalid(
                format!("{name} names {} pages; at most {MAX_ORDER_PAGES}", pages.len()),
                vec![name],
            ));
        }
        let pages = pages
            .iter()
            .map(|page| {
                page.as_str().filter(|page| identifier(page)).map(str::to_owned).ok_or_else(|| {
                    OrderRefusal::invalid(format!("{name}: page names must be non-empty sub-panel identifiers"), vec![name])
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        order.push((edge, pages));
    }
    check_unique(&order)?;
    Ok(order)
}

/// A page may be declared once in the whole file.
fn check_unique(order: &[(ShellEdge, Vec<String>)]) -> Result<(), OrderRefusal> {
    let mut seen: std::collections::BTreeMap<&str, ShellEdge> = std::collections::BTreeMap::new();
    for (edge, pages) in order {
        for page in pages {
            if let Some(first) = seen.insert(page.as_str(), *edge) {
                let name = edge_name(*edge);
                return Err(if first == *edge {
                    OrderRefusal::invalid(format!("page {page} is named twice on {name}"), vec![name])
                } else {
                    OrderRefusal::invalid(format!("page {page} is named on two edges"), vec![edge_name(first), name])
                });
            }
        }
    }
    Ok(())
}

/// `shell.panel.order`'s one write: replace `panels.<edge>` for every edge in
/// `order` in a single atomic file replacement, so moving a page between
/// edges is never half-applied. Edges not named keep their declarations, and
/// a page they declare may not also appear in `order`. Other keys' values are
/// kept; comments and formatting are not (as in Quoin). Returns every edge's
/// declarations after the write.
pub fn write_order(path: &Path, order: &[(ShellEdge, Vec<String>)]) -> Result<Declared, OrderRefusal> {
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "{}".to_owned(),
        Err(error) => return Err(OrderRefusal::write(format!("could not read conf.mix: {error}"))),
    };
    let mut value = parse_mix_data(&source).map_err(|error| OrderRefusal::write(format!("current conf.mix is invalid: {error}")))?;
    let current = declared_of(&value).map_err(|error| OrderRefusal::write(format!("current conf.mix is invalid: {error}")))?;
    let mut merged = current;
    for (edge, pages) in order {
        merged[edge.index()] = pages.clone();
    }
    check_unique(&ShellEdge::ALL.map(|edge| (edge, merged[edge.index()].clone())))?;
    let Value::Map(root) = &mut value else {
        return Err(OrderRefusal::write("conf.mix is not a map".to_owned()));
    };
    let panels = root.entry("panels".to_owned()).or_insert_with(|| Value::Map(Default::default()));
    let Value::Map(panels) = panels else {
        return Err(OrderRefusal::write("conf.mix panels is not a map".to_owned()));
    };
    for (edge, pages) in order {
        panels.insert(edge_name(*edge).to_owned(), Value::List(pages.iter().cloned().map(Value::String).collect()));
    }
    let encoded = value.encode_pretty().map_err(|error| OrderRefusal::write(error.to_string()))?;
    let reread = parse_mix_data(&encoded)
        .map_err(|error| error.to_string())
        .and_then(|value| declared_of(&value))
        .map_err(|error| OrderRefusal::write(format!("re-encoded conf.mix is invalid: {error}")))?;
    replace_atomically(path, &encoded).map_err(|error| OrderRefusal::write(format!("could not replace conf.mix: {error}")))?;
    Ok(reread)
}

/// Write `encoded` beside `path` and rename it over, so a reader only ever
/// sees a complete file. A symlinked conf.mix stays a link (its target is
/// replaced). The data is fsynced before the rename and the directory after.
pub(crate) fn replace_atomically(path: &Path, encoded: &str) -> Result<(), String> {
    let target = if path.is_symlink() { std::fs::canonicalize(path).map_err(|error| error.to_string())? } else { path.to_path_buf() };
    replace_atomically_at(&target, encoded)
}

/// Replace this exact directory entry. State files must not follow a link
/// into another state directory; only conf.mix supports linked targets.
pub(crate) fn replace_atomically_at(target: &Path, encoded: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let parent = target.parent().ok_or("config path has no parent")?;
    let parent = if parent.as_os_str().is_empty() { Path::new(".") } else { parent };
    let permissions = match std::fs::symlink_metadata(target) {
        Ok(metadata) if metadata.is_symlink() => return Err("refusing to replace a symlinked state file".into()),
        Ok(metadata) => metadata.permissions(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::Permissions::from_mode(0o600),
        Err(error) => return Err(error.to_string()),
    };
    create_synced_dirs(parent).map_err(|error| error.to_string())?;
    let (temp, mut file) = create_temp(target, &NEXT_TEMP).map_err(|error| error.to_string())?;
    let written = (|| -> std::io::Result<()> {
        file.write_all(encoded.as_bytes())?;
        file.set_permissions(permissions)?;
        file.sync_all()?;
        std::fs::rename(&temp, target)?;
        std::fs::File::open(parent)?.sync_all()
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written.map_err(|error| error.to_string())
}

fn create_synced_dirs(path: &Path) -> std::io::Result<()> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => return Ok(()),
        Ok(_) => return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "config parent is not a directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    create_synced_dirs(parent)?;
    match std::fs::create_dir(path) {
        Ok(()) => std::fs::File::open(parent)?.sync_all(),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(error) => Err(error),
    }
}

fn create_temp(target: &Path, next: &AtomicU64) -> std::io::Result<(PathBuf, std::fs::File)> {
    use std::os::unix::fs::OpenOptionsExt;
    let parent = target.parent().ok_or_else(|| std::io::Error::other("config path has no parent"))?;
    let name = target.file_name().and_then(|name| name.to_str()).unwrap_or("conf.mix");
    loop {
        let serial = next.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".{name}.{}.{serial}.tmp", std::process::id()));
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scene-host-conf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn exclusive_temps_skip_existing_files_and_symlinks() {
        use std::io::Write;
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = temp_dir("exclusive");
        let target = dir.join("conf.mix");
        let occupied = dir.join(format!(".conf.mix.{}.0.tmp", std::process::id()));
        let link = dir.join(format!(".conf.mix.{}.1.tmp", std::process::id()));
        let victim = dir.join("victim");
        std::fs::write(&occupied, "occupied").unwrap();
        std::fs::write(&victim, "untouched").unwrap();
        symlink(&victim, &link).unwrap();
        let next = AtomicU64::new(0);
        let (temp, mut file) = create_temp(&target, &next).unwrap();
        assert_eq!(temp, dir.join(format!(".conf.mix.{}.2.tmp", std::process::id())));
        file.write_all(b"replacement").unwrap();
        assert_eq!(std::fs::read_to_string(occupied).unwrap(), "occupied");
        assert_eq!(std::fs::read_to_string(victim).unwrap(), "untouched");
        assert!(link.is_symlink());
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn replacement_preserves_permissions_and_a_symlinked_configs_target() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = temp_dir("permissions");
        let target = dir.join("new/nested/conf.mix");
        replace_atomically(&target, "{}").unwrap();
        assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777, 0o600);
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        let link = dir.join("conf.mix");
        symlink(&target, &link).unwrap();
        replace_atomically(&link, "{panels: {}}").unwrap();
        assert!(link.is_symlink());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "{panels: {}}");
        assert_eq!(std::fs::metadata(link).unwrap().permissions().mode() & 0o7777, 0o640);
    }

    #[test]
    fn the_frozen_order_is_written_merged_and_read_back() {
        let frozen: serde_json::Value = serde_json::from_str(include_str!("../tests/fixtures/shell-verbs.json")).unwrap();
        let fixture = &frozen["shell.panel.order"];
        let path = temp_dir("frozen").join("quoin/conf.mix");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{carousel_motion: \"slide\", panels: {top: [\"clock\"]}}\n").unwrap();
        let order = parse_order(&fixture["request"]).unwrap();
        let written = write_order(&path, &order).unwrap();
        assert_eq!(written[ShellEdge::Right.index()], ["scene-notes", "settings.appearance"]);
        assert_eq!(written[ShellEdge::Left.index()], ["scene-launcher", "scene-calendar"]);
        assert_eq!(written[ShellEdge::Top.index()], ["clock"], "an edge not named keeps its declarations");
        assert_eq!(read_declared(&path).unwrap(), written);
        let again = parse_mix_data(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let Value::Map(root) = &again else { panic!("map") };
        assert!(matches!(root.get("carousel_motion"), Some(Value::String(motion)) if motion == "slide"), "other keys survive");
    }

    #[test]
    fn refusals_match_the_frozen_shape() {
        let frozen: serde_json::Value = serde_json::from_str(include_str!("../tests/fixtures/shell-verbs.json")).unwrap();
        let both = json!({"edges": {"left": ["scene-notes"], "right": ["scene-notes", "settings.appearance"]}});
        assert_eq!(parse_order(&both).unwrap_err().body(), frozen["shell.panel.order"]["refusals"]["INVALID_ARGUMENT"]);
        assert_eq!(parse_order(&json!({"edges": {}})).unwrap_err().code, "INVALID_ARGUMENT");
        assert_eq!(parse_order(&json!({"edges": {"middle": []}})).unwrap_err().code, "INVALID_ARGUMENT");
        assert_eq!(parse_order(&json!({"edges": {"left": ["a b"]}})).unwrap_err().code, "INVALID_ARGUMENT");
        // A page another edge already declares in the file cannot be named.
        let path = temp_dir("conflict").join("conf.mix");
        std::fs::write(&path, "{panels: {top: [\"scene-notes\"]}}\n").unwrap();
        let order = parse_order(&json!({"edges": {"left": ["scene-notes"]}})).unwrap();
        assert_eq!(write_order(&path, &order).unwrap_err().code, "INVALID_ARGUMENT");
        // A missing file declares nothing and is created by the write.
        let fresh = temp_dir("fresh").join("quoin/conf.mix");
        assert_eq!(read_declared(&fresh).unwrap(), Declared::default());
        assert_eq!(write_order(&fresh, &order).unwrap()[ShellEdge::Left.index()], ["scene-notes"]);
    }
}
