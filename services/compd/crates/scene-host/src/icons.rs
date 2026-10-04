//! Local freedesktop icon lookup for scene images. The apps service usually
//! supplies a path; names and missing paths also need a useful icon. iced
//! loads SVG/PNG itself, at the surface's output scale.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub(crate) fn is_svg(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
}

fn image_file(path: &Path) -> bool {
    path.is_file() && path.extension().is_some_and(|ext|
        ["svg", "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"].iter().any(|kind| ext.eq_ignore_ascii_case(kind)))
}

type Ini = BTreeMap<String, BTreeMap<String, String>>;
type IconCache = BTreeMap<(String, u32, Option<String>), PathBuf>;

fn ini(path: &Path) -> Ini {
    let mut sections = Ini::new();
    let mut section = String::new();
    for line in std::fs::read_to_string(path).unwrap_or_default().lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') { continue; }
        if let Some(name) = line.strip_prefix('[').and_then(|line| line.strip_suffix(']')) {
            section = name.into();
        } else if let Some((key, value)) = line.split_once('=') {
            sections.entry(section.clone()).or_default().insert(key.trim().into(), value.trim().trim_matches('"').into());
        }
    }
    sections
}

struct Lookup {
    roots: Vec<PathBuf>,
    pixmaps: Vec<PathBuf>,
    applications: Vec<PathBuf>,
    theme: String,
}

impl Lookup {
    fn system() -> Self {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
        let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
        let mut roots = vec![home.join(".icons"), data.join("icons")];
        let mut pixmaps = vec![data.join("pixmaps")];
        let mut applications = vec![data.join("applications")];
        let dirs = std::env::var_os("XDG_DATA_DIRS").filter(|dirs| !dirs.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        for dir in std::env::split_paths(&dirs) {
            roots.push(dir.join("icons"));
            pixmaps.push(dir.join("pixmaps"));
            applications.push(dir.join("applications"));
        }
        let gtk = ["gtk-4.0/settings.ini", "gtk-3.0/settings.ini"].into_iter().find_map(|file| {
            ini(&config.join(file)).get("Settings").and_then(|s| s.get("gtk-icon-theme-name")).cloned()
        });
        let kde = ini(&config.join("kdeglobals")).get("Icons").and_then(|s| s.get("Theme")).cloned();
        let theme = std::env::var("XDG_ICON_THEME").ok().or(gtk).or(kde).unwrap_or_else(|| "Adwaita".into());
        Self { roots, pixmaps, applications, theme }
    }

    fn application_icon(&self, id: &str) -> Option<String> {
        if id.is_empty() || id.contains(['/', '\\']) || id == "." || id == ".." { return None; }
        let file = if id.ends_with(".desktop") { id.to_owned() } else { format!("{id}.desktop") };
        self.applications.iter().find_map(|dir| {
            let direct = dir.join(&file);
            let path = if direct.is_file() { Some(direct) } else { desktop_file(dir, dir, &file, 0) }?;
            let entry = ini(&path);
            let fields = entry.get("Desktop Entry")?;
            fields.get("Icon").filter(|icon| !icon.is_empty()).cloned()
                // Some installed desktop files have
                // no Icon=. Honour their declared application category.
                .or_else(|| fields.get("Categories").and_then(|categories| {
                    theme_list(categories).find_map(|category| match category {
                        "AudioVideo" => Some("applications-multimedia".into()),
                        _ => None,
                    })
                }))
        })
    }

    fn find(&self, name: &str, size: u32) -> Option<PathBuf> {
        let name = name.trim().strip_prefix("file://").unwrap_or(name.trim());
        let path = Path::new(name);
        if path.is_absolute() && image_file(path) { return Some(path.into()); }
        // A name is one filename, never a route out of an icon theme.
        if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." { return None; }
        let mut seen = BTreeSet::new();
        for theme in [&self.theme, "hicolor", "Adwaita", "AdwaitaLegacy"] {
            if let Some(path) = self.theme_icon(theme, name, size, &mut seen) { return Some(path); }
        }
        for dir in &self.pixmaps {
            if let Some(path) = icon_in(dir, name) { return Some(path); }
        }
        // Adwaita installs shell glyphs exclusively as *-symbolic. Names
        // from Breeze (the behaviour's default theme) need that fallback.
        let stem = name.strip_suffix(".svg").or_else(|| name.strip_suffix(".png")).unwrap_or(name);
        if !stem.ends_with("-symbolic") {
            let symbolic = format!("{stem}-symbolic");
            if let Some(path) = self.find(&symbolic, size) { return Some(path); }
        } else if let Some(plain) = stem.strip_suffix("-symbolic") {
            // Do not recurse back to symbolic after a plain miss.
            let mut seen = BTreeSet::new();
            for theme in [&self.theme, "hicolor", "Adwaita", "AdwaitaLegacy"] {
                if let Some(path) = self.theme_icon(theme, plain, size, &mut seen) { return Some(path); }
            }
        }
        // Equivalent Freedesktop shell glyphs across KDE/GNOME themes.
        let alias = match stem.strip_suffix("-symbolic").unwrap_or(stem) {
            "notifications" | "notification-active" | "notification-inactive" => Some("preferences-system-notifications-symbolic"),
            "network-wired-activated" => Some("network-wired-symbolic"),
            "desktop" | "user-desktop" => Some("video-display-symbolic"),
            "display" => Some("video-display-symbolic"),
            _ => None,
        };
        if let Some(alias) = alias { return self.find(alias, size); }
        None
    }

    fn theme_icon(&self, theme: &str, name: &str, size: u32, seen: &mut BTreeSet<String>) -> Option<PathBuf> {
        if theme.is_empty() || theme.contains(['/', '\\']) || !seen.insert(theme.into()) || seen.len() > 32 { return None; }
        let mut candidates = Vec::new();
        let mut inherited = Vec::new();
        for root in &self.roots {
            let base = root.join(theme);
            let index = ini(&base.join("index.theme"));
            if let Some(header) = index.get("Icon Theme") {
                if let Some(names) = header.get("Inherits") {
                    inherited.extend(theme_list(names).map(str::to_owned));
                }
                for key in ["Directories", "ScaledDirectories"] {
                    for dir in header.get(key).into_iter().flat_map(|dirs| theme_list(dirs)) {
                        // Only the directories declared by the theme, with no
                        // traversal or recursive scan of thousands of icons.
                        if !Path::new(dir).components().all(|part| matches!(part, std::path::Component::Normal(_))) { continue; }
                        let fields = index.get(dir);
                        let number = |key: &str, default: u32| fields.and_then(|s| s.get(key)).and_then(|n| n.parse::<u32>().ok()).unwrap_or(default);
                        let nominal = number("Size", size);
                        let scale = number("Scale", 1).max(1);
                        let kind = fields.and_then(|s| s.get("Type")).map(String::as_str).unwrap_or("Threshold");
                        let (min, max) = match kind {
                            "Scalable" => (number("MinSize", nominal), number("MaxSize", nominal)),
                            "Fixed" => (nominal, nominal),
                            _ => (nominal.saturating_sub(number("Threshold", 2)), nominal.saturating_add(number("Threshold", 2))),
                        };
                        let distance = min.saturating_mul(scale).saturating_sub(size).max(size.saturating_sub(max.saturating_mul(scale)));
                        candidates.push((distance, base.join(dir)));
                    }
                }
            }
            // Packages and user overlays can install directories omitted by
            // index.theme. Inspect directory names, not every icon file. This
            // also covers unindexed themes, uncommon sizes and legacy contexts.
            let mut installed = Vec::new();
            for entry in std::fs::read_dir(&base).into_iter().flatten().flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let distance = if matches!(name.as_ref(), "scalable" | "symbolic") { 0 } else {
                    let (dimensions, scale) = name.split_once('@').unwrap_or((&name, "1"));
                    let Some((w, h)) = dimensions.split_once('x') else { continue };
                    let (Ok(w), Ok(h), Ok(scale)) = (w.parse::<u32>(), h.parse::<u32>(), scale.parse::<u32>()) else { continue };
                    if w != h || scale == 0 { continue; }
                    w.saturating_mul(scale).abs_diff(size)
                };
                for context in std::fs::read_dir(entry.path()).into_iter().flatten().flatten() {
                    if context.file_type().is_ok_and(|kind| kind.is_dir()) {
                        installed.push((distance, context.path()));
                    }
                }
            }
            installed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            candidates.extend(installed);
            candidates.push((u32::MAX, base));
        }
        // Stable sort keeps XDG root order when sizes match.
        candidates.sort_by_key(|(distance, _)| *distance);
        for (_, dir) in candidates {
            if let Some(path) = icon_in(&dir, name) { return Some(path); }
        }
        for theme in inherited {
            if let Some(path) = self.theme_icon(&theme, name, size, seen) { return Some(path); }
        }
        None
    }
}

fn theme_list(value: &str) -> impl Iterator<Item = &str> {
    value.split(|c: char| c == ',' || c == ';' || c.is_whitespace()).filter(|name| !name.is_empty())
}

// Freedesktop desktop IDs flatten subdirectories with '-'. Only descend into
// real directories, so symlinks cannot introduce cycles or escape the root.
fn desktop_file(root: &Path, dir: &Path, id: &str, depth: usize) -> Option<PathBuf> {
    if depth > 8 { return None; }
    let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            if let Some(path) = desktop_file(root, &path, id, depth + 1) { return Some(path); }
        } else if path.strip_prefix(root).ok()?.to_string_lossy().replace('/', "-") == id {
            return Some(path);
        }
    }
    None
}

fn icon_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let path = dir.join(name);
    if image_file(&path) { return Some(path); }
    // Icon= may include an extension even when the installed variant uses
    // another one. Never append '.png' to 'firefox.svg'.
    let stem = Path::new(name).extension().filter(|ext| ["svg", "png"].iter().any(|kind| ext.eq_ignore_ascii_case(kind)))
        .and_then(|_| Path::new(name).file_stem()).and_then(|stem| stem.to_str()).unwrap_or(name);
    ["png", "svg"].into_iter().map(|ext| dir.join(format!("{stem}.{ext}"))).find(|path| image_file(path))
}

impl Lookup {
    fn resolve(&self, source: &str, size: u32, app: Option<&str>) -> Option<PathBuf> {
        self.find(source, size)
            .or_else(|| app.and_then(|id| self.find(id.trim_end_matches(".desktop"), size)))
            .or_else(|| app.and_then(|id| self.application_icon(id)).and_then(|name| self.find(&name, size)))
            // Recover an obsolete service path by its icon filename, too.
            .or_else(|| Path::new(source).file_name().and_then(|name| name.to_str()).and_then(|name| self.find(name, size)))
            .or_else(|| self.find("application-x-executable", size))
            .or_else(|| self.find("application-default-icon", size))
    }
}

pub(crate) fn resolve(source: &str, size: u32, app: Option<&str>) -> Option<PathBuf> {
    static LOOKUP: OnceLock<Lookup> = OnceLock::new();
    static CACHE: OnceLock<Mutex<IconCache>> = OnceLock::new();
    let source = source.trim().strip_prefix("file://").unwrap_or(source.trim());
    let path = Path::new(source);
    if path.is_absolute() && image_file(path) { return Some(path.into()); }
    let key = (source.to_owned(), size, app.map(str::to_owned));
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(path) = cache.lock().ok().and_then(|cache| cache.get(&key).filter(|path| image_file(path)).cloned()) { return Some(path); }
    let lookup = LOOKUP.get_or_init(Lookup::system);
    let path = lookup.resolve(source, size, app);
    if let Some(path) = &path && let Ok(mut cache) = cache.lock() {
        if cache.len() >= 512 { cache.clear(); }
        cache.insert(key, path.clone());
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_names_recover_adwaita_symbolic_glyphs_and_suffixes() {
        let scratch = Scratch::new();
        for name in ["preferences-system-notifications", "audio-volume-high", "network-wired", "video-display"] {
            scratch.write(&format!("Adwaita/symbolic/status/{name}-symbolic.svg"), "svg");
        }
        scratch.write("Adwaita/scalable/apps/plain.svg", "svg");
        let lookup = Lookup { roots: vec![scratch.0.clone()], pixmaps: vec![], applications: vec![], theme: "MissingBreeze".into() };
        for (name, expected) in [("notifications", "preferences-system-notifications"), ("audio-volume-high", "audio-volume-high"),
            ("network-wired-activated", "network-wired"), ("user-desktop", "video-display"), ("display", "video-display")]
        {
            assert_eq!(lookup.find(name, 20), Some(scratch.0.join(format!("Adwaita/symbolic/status/{expected}-symbolic.svg"))));
        }
        assert_eq!(lookup.find("plain-symbolic", 20), Some(scratch.0.join("Adwaita/scalable/apps/plain.svg")));
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let root = Path::new(option_env!("CARGO_MANIFEST_DIR").unwrap_or("services/compd/crates/scene-host"))
                .join("tests").join(format!(".tmp-icons-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn write(&self, path: &str, data: &str) {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) { std::fs::remove_dir_all(&self.0).unwrap(); }
    }

    #[test]
    fn themed_lookup_inherits_and_chooses_scalable_or_nearest_sized_icons() {
        let scratch = Scratch::new();
        let root = &scratch.0;
        let write = |path: &str, data: &str| scratch.write(path, data);
        write("Custom/index.theme", "[Icon Theme]\nInherits=hicolor\nDirectories=32x32/apps,64x64/apps\n[32x32/apps]\nSize=32\nType=Fixed\n[64x64/apps]\nSize=64\nType=Fixed\n");
        write("Custom/32x32/apps/app.png", "png");
        write("Custom/64x64/apps/app.png", "png");
        write("hicolor/index.theme", "[Icon Theme]\nDirectories=scalable/apps\n[scalable/apps]\nSize=48\nType=Scalable\nMinSize=16\nMaxSize=512\n");
        write("hicolor/scalable/apps/inherited.svg", "svg");
        write("applications/app.desktop", "[Desktop Entry]\nIcon=app\n");
        let lookup = Lookup { roots: vec![root.clone()], pixmaps: vec![], applications: vec![root.join("applications")], theme: "Custom".into() };
        assert_eq!(lookup.find("app", 32), Some(root.join("Custom/32x32/apps/app.png")));
        assert_eq!(lookup.find("app", 60), Some(root.join("Custom/64x64/apps/app.png")));
        assert_eq!(lookup.find("inherited", 80), Some(root.join("hicolor/scalable/apps/inherited.svg")));
        assert!(lookup.find("../escape", 32).is_none());
        assert_eq!(lookup.application_icon("app"), Some("app".into()));
        assert!(is_svg(Path::new("icon.SVG")));
    }

    #[test]
    fn launcher_recovers_all_missing_desktop_icons_across_arch_theme_layouts() {
        let scratch = Scratch::new();
        let root = &scratch.0;
        // Selected theme missing; a real index can omit package-added sizes.
        scratch.write("icons/hicolor/index.theme", "[Icon Theme]\nDirectories=16x16/apps; scalable/apps\n[16x16/apps]\nSize=16\nType=Fixed\n[scalable/apps]\nSize=48\nType=Scalable\nMinSize=16\nMaxSize=512\n");
        scratch.write("icons/hicolor/32x32/apps/firefox.png", "png");
        scratch.write("icons/hicolor/192x192/apps/firefox.png", "png");
        scratch.write("icons/hicolor/512x512/apps/org.mozilla.Thunderbird.png", "png");
        scratch.write("icons/hicolor/scalable/apps/foot.svg", "svg");
        scratch.write("icons/hicolor/scalable/apps/rio.svg", "svg");
        scratch.write("icons/Adwaita/index.theme", "[Icon Theme]\nInherits=AdwaitaLegacy; hicolor\n");
        scratch.write("icons/AdwaitaLegacy/32x32/legacy/network-wired.png", "png");
        let lookup = Lookup {
            roots: vec![root.join("icons")], pixmaps: vec![root.join("pixmaps")],
            applications: vec![root.join("applications")], theme: "NotInstalled".into(),
        };
        for (id, icon, expected) in [
            ("bssh", "network-wired", "AdwaitaLegacy/32x32/legacy/network-wired.png"),
            ("bvnc", "network-wired", "AdwaitaLegacy/32x32/legacy/network-wired.png"),
            ("avahi-discover", "network-wired", "AdwaitaLegacy/32x32/legacy/network-wired.png"),
            ("firefox", "firefox", "hicolor/32x32/apps/firefox.png"),
            ("footclient", "foot", "hicolor/scalable/apps/foot.svg"),
            ("foot-server", "foot", "hicolor/scalable/apps/foot.svg"),
            ("foot", "foot", "hicolor/scalable/apps/foot.svg"),
            ("rio", "rio", "hicolor/scalable/apps/rio.svg"),
            ("org.mozilla.Thunderbird", "org.mozilla.Thunderbird", "hicolor/512x512/apps/org.mozilla.Thunderbird.png"),
        ] {
            scratch.write(&format!("applications/{id}.desktop"), &format!("[Desktop Entry]\nIcon={icon}\n"));
            let expected = Some(root.join("icons").join(expected));
            assert_eq!(lookup.resolve("", 32, Some(id)), expected, "{id}: empty service result");
            assert_eq!(lookup.resolve("/missing/service/icon.png", 32, Some(&format!("{id}.desktop"))), expected, "{id}: stale service path");
            assert_eq!(lookup.resolve(icon, 32, None), expected, "{id}: icon name");
        }
        assert_eq!(lookup.find("firefox", 190), Some(root.join("icons/hicolor/192x192/apps/firefox.png")));
        assert_eq!(lookup.find("firefox.svg", 32), Some(root.join("icons/hicolor/32x32/apps/firefox.png")));
        assert_eq!(lookup.find("foot.png", 32), Some(root.join("icons/hicolor/scalable/apps/foot.svg")));
        assert_eq!(lookup.resolve("/old/icons/firefox.png", 32, None), Some(root.join("icons/hicolor/32x32/apps/firefox.png")));
    }

    #[test]
    fn pixmaps_absolute_icon_paths_and_nested_desktop_ids_are_recovered() {
        let scratch = Scratch::new();
        let root = std::fs::canonicalize(&scratch.0).unwrap();
        scratch.write("pixmaps/pixmap-app.png", "png");
        scratch.write("private/absolute.svg", "svg");
        scratch.write("applications/vendor/pixmap.desktop", "[Desktop Entry]\nIcon=pixmap-app.png\n");
        let absolute = root.join("private/absolute.svg");
        scratch.write("applications/absolute.desktop", &format!("[Desktop Entry]\nIcon={}\n", absolute.display()));
        let lookup = Lookup {
            roots: vec![root.join("icons")], pixmaps: vec![root.join("pixmaps")],
            applications: vec![root.join("applications")], theme: "Missing".into(),
        };
        assert_eq!(lookup.resolve("", 32, Some("vendor-pixmap")), Some(root.join("pixmaps/pixmap-app.png")));
        assert_eq!(lookup.resolve("", 32, Some("absolute")), Some(absolute.clone()));
        assert_eq!(lookup.resolve(absolute.to_str().unwrap(), 32, None), Some(absolute.clone()));
        assert_eq!(lookup.resolve(&format!("file://{}", absolute.display()), 32, None), Some(absolute));
        for name in ["../escape", "../../escape", "/missing.svg"] {
            assert!(lookup.find(name, 32).is_none());
        }
        assert!(lookup.application_icon("../escape").is_none());
    }

    #[test]
    fn desktop_id_and_category_recover_media_without_an_icon_field() {
        let scratch = Scratch::new();
        scratch.write("applications/dev.mixos.media.desktop", "[Desktop Entry]\nName=Media\nCategories=AudioVideo;Player;\n");
        scratch.write("icons/Adwaita/symbolic/categories/applications-multimedia-symbolic.svg", "svg");
        let lookup = Lookup {
            roots: vec![scratch.0.join("icons")], pixmaps: vec![],
            applications: vec![scratch.0.join("applications")], theme: "Adwaita".into(),
        };
        let expected = scratch.0.join("icons/Adwaita/symbolic/categories/applications-multimedia-symbolic.svg");
        assert_eq!(lookup.resolve("", 32, Some("dev.mixos.media.desktop")), Some(expected));
        // A dedicated application icon wins over its category fallback.
        scratch.write("icons/hicolor/scalable/apps/dev.mixos.media.svg", "svg");
        assert_eq!(lookup.resolve("", 32, Some("dev.mixos.media.desktop")),
            Some(scratch.0.join("icons/hicolor/scalable/apps/dev.mixos.media.svg")));
    }

    #[test]
    #[ignore = "requires COMPD_ICON_ROOT pointing to an installed icon tree"]
    fn installed_icons_resolve_without_generic_placeholders() {
        let root = PathBuf::from(std::env::var_os("COMPD_ICON_ROOT").expect("COMPD_ICON_ROOT"));
        let lookup = Lookup {
            roots: vec![root.join("usr/local/share/icons"), root.join("usr/share/icons")],
            pixmaps: vec![root.join("usr/local/share/pixmaps"), root.join("usr/share/pixmaps")],
            applications: vec![root.join("usr/local/share/applications"), root.join("usr/share/applications")],
            theme: "Adwaita".into(),
        };
        for id in ["bssh", "bvnc", "avahi-discover", "dev.mixos.media"] {
            let path = lookup.resolve("", 32, Some(id)).unwrap_or_else(|| panic!("missing {id}"));
            let stem = path.file_stem().unwrap().to_string_lossy();
            assert!(!matches!(stem.as_ref(), "application-x-executable" | "application-default-icon"), "{id}: {}", path.display());
            assert!(path.is_file());
            println!("{id}: {}", path.display());
        }
    }
}
