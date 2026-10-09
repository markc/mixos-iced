//! Prepare small scene icons before drawing. File-backed raster handles load
//! asynchronously in iced; a static compositor page can lose their redraw to
//! another surface consuming the shared notifier. RGBA handles load in the
//! layout pass, so the first frame already has pixels and valid dimensions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use iced_core::image::Handle;

#[derive(Clone)]
pub(crate) enum Asset {
    Svg(iced_core::svg::Handle, bool),
    Raster(Handle),
}

pub(crate) type Prepared = BTreeMap<(String, String, String), Asset>;
type AssetCache = BTreeMap<(String, u32, Option<String>), Asset>;

pub(crate) fn key(id: &str, row: Option<&crate::view::Route>) -> (String, String, String) {
    (row.map_or("", |r| r.node.as_str()).into(),
        row.and_then(|r| r.item.as_ref()).and_then(|item| item["id"].as_str()).unwrap_or_default().into(), id.into())
}

// The panel behaviour publishes an empty src and hides the image when apps
// cannot resolve its requested theme. Recover these shell glyphs locally;
// never replace a deliberately hidden application image.
pub(crate) fn panel_fallback(id: &str) -> Option<&'static str> {
    match id {
        "st_notes_i" => Some("preferences-system-notifications-symbolic"),
        "st_vol_i" => Some("audio-volume-high-symbolic"),
        "st_net_i" => Some("network-wired-symbolic"),
        "peek_i" => Some("video-display-symbolic"),
        // The launcher mark, the full-colour cyclops (share/brand installs
        // it). Not symbolic: its red eye must keep its colour.
        "launcher_icon" => Some("dev.mixos"),
        _ => None,
    }
}

pub(crate) fn fallback_image(id: &str) -> Option<&'static str> {
    match id {
        "st_notes_f" => Some("st_notes_i"), "st_vol_f" => Some("st_vol_i"),
        "st_net_f" => Some("st_net_i"), "peek_f" => Some("peek_i"),
        "launcher_text" => Some("launcher_icon"), _ => None,
    }
}

pub(crate) fn prepare(tree: &scene::ResolvedScene, lists: &crate::templates::PreparedLists) -> Prepared {
    // Only the fixed production resolver shares this cache. Injected resolvers
    // must use their own cache so they cannot consume or replace its assets.
    static CACHE: OnceLock<Mutex<AssetCache>> = OnceLock::new();
    prepare_with(tree, lists, CACHE.get_or_init(Mutex::default), crate::icons::resolve)
}

fn prepare_with(
    tree: &scene::ResolvedScene,
    lists: &crate::templates::PreparedLists,
    cache: &Mutex<AssetCache>,
    resolve: impl Fn(&str, u32, Option<&str>) -> Option<PathBuf>,
) -> Prepared {
    let mut prepared = Prepared::new();
    let mut insert = |key, id: &str, node: &scene::Node, app: Option<&str>| {
        if node.family != "image" { return; }
        let source = crate::templates::text(node, "src");
        let source = if source.is_empty() && tree.name == "panel" {
            panel_fallback(id).unwrap_or(source)
        } else { source };
        // Launcher services can leave src empty or return an obsolete path.
        // Keep the desktop ID available so local theme lookup can recover it.
        if source.is_empty() && app.is_none() { return; }
        let size = ["w", "h"].map(|p| node.ports.get(p).and_then(serde_json::Value::as_f64).unwrap_or(16.0));
        let size = size[0].max(size[1]).ceil() as u32;
        let cache_key = (source.to_owned(), size, app.map(str::to_owned));
        if let Some(asset) = cache.lock().ok().and_then(|cache| cache.get(&cache_key).cloned()) {
            prepared.insert(key, asset);
            return;
        }
        if let Some(path) = resolve(source, size, app) {
            let asset = if crate::icons::is_svg(&path) {
                // Keep bytes and identity alive through layout/page switches.
                std::fs::read(&path).ok().map(|bytes| Asset::Svg(iced_core::svg::Handle::from_memory(bytes), theme_glyph(&path)))
            } else { raster(&path).map(Asset::Raster) };
            if let Some(asset) = asset {
                if let Ok(mut cache) = cache.lock() && cache.len() < 512 {
                    cache.insert(cache_key, asset.clone());
                }
                prepared.insert(key, asset);
            }
        }
    };
    let templates = crate::templates::template_ids(tree);
    for (id, node) in &tree.nodes {
        if !templates.contains(id) { insert(key(id, None), id, node, None); }
    }
    for (list, data) in lists {
        for (item, nodes) in &data.instances {
            let app = tree.nodes.get(list).and_then(|node| crate::templates::rows(node).iter().find(|row| row["id"].as_str() == Some(item.as_str())))
                .and_then(|row| row["app_id"].as_str().or_else(|| row["id"].as_str()));
            for (id, node) in nodes { insert((list.clone(), item.clone(), id.clone()), id, node, app); }
        }
    }
    prepared
}

type RasterCache = BTreeMap<PathBuf, (Option<SystemTime>, u64, Handle)>;

pub(crate) fn raster(path: &Path) -> Option<Handle> {
    static CACHE: OnceLock<Mutex<RasterCache>> = OnceLock::new();
    raster_with_cache(path, CACHE.get_or_init(Mutex::default))
}

fn raster_with_cache(path: &Path, cache: &Mutex<RasterCache>) -> Option<Handle> {
    let metadata = path.metadata().ok()?;
    let stamp = (metadata.modified().ok(), metadata.len());
    // Keep lookup, decoding and insertion atomic: concurrent misses must not
    // create different IDs for the same unchanged file.
    let mut cache = cache.lock().ok()?;
    if let Some((_, _, handle)) = cache.get(path)
        .filter(|(modified, len, _)| (*modified, *len) == stamp)
    {
        return Some(handle.clone());
    }
    let pixels = iced_graphics::image::load(&Handle::from_path(path)).ok()?;
    let handle = Handle::from_rgba(pixels.width(), pixels.height(), pixels.into_raw());
    if cache.len() >= 512 {
        // A live handle shares its pixel buffer with the cache. Only discard
        // unused entries, allowing the limit to grow while scenes hold icons.
        cache.retain(|_, (_, _, handle)| {
            matches!(handle, Handle::Rgba { pixels, .. } if !pixels.is_unique())
        });
    }
    cache.insert(path.into(), (stamp.0, stamp.1, handle.clone()));
    Some(handle)
}

/// Symbolic icons and KDE's monochrome theme glyphs need the scene ink. Apps
/// retain their artwork; a light-theme network glyph must not disappear on
/// the dark launcher just because apps.icon supplied its absolute path.
pub(crate) fn theme_glyph(path: &Path) -> bool {
    if path
        .file_stem()
        .is_some_and(|name| name.to_string_lossy().ends_with("-symbolic"))
    {
        return true;
    }
    if path.components().any(|part| part.as_os_str() == "apps") {
        return false;
    }
    std::fs::read_to_string(path).is_ok_and(|source| source.contains("ColorScheme-Text"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_launcher_sources_reach_desktop_lookup_and_prepare_real_pixels() {
        let document = scene::parse(
            r#"---
scene: 1
name: launcher
citizen: test
---
```mix
root: {widget: "column", children: ["apps"]}
apps: {widget: "list", row: "app", row_height: 31, rows: [
  {id: "dev.mixos.media.desktop", cells: []},
  {id: "bssh.desktop", cells: []},
  {id: "bvnc.desktop", cells: []},
  {id: "avahi-discover.desktop", cells: []}
]}
app: {widget: "row", children: ["icon"]}
icon: {widget: "image", src: "", w: 31, h: 31}
```
"#,
        ).expect("empty-source launcher scene must parse");
        let tree = scene::resolve(&document)
            .expect("empty-source launcher scene must resolve");
        let lists = crate::templates::validate_templates(&tree)
            .expect("empty-source launcher row templates must validate");
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/icon.png");
        let seen = std::cell::RefCell::new(Vec::new());
        let cache = Mutex::new(AssetCache::new());
        let images = prepare_with(&tree, &lists, &cache, |source, _, app| {
            assert!(source.is_empty());
            seen.borrow_mut().push(app.expect("launcher image must retain its desktop ID").to_owned());
            Some(path.clone())
        });
        assert_eq!(seen.borrow().len(), 4, "empty sources must not bypass recovery");
        for id in ["dev.mixos.media.desktop", "bssh.desktop", "bvnc.desktop", "avahi-discover.desktop"] {
            assert!(matches!(images.get(&("apps".into(), id.into(), "icon".into())), Some(Asset::Raster(Handle::Rgba { .. }))));
        }
    }

    #[test]
    fn empty_panel_launcher_recovers_the_full_colour_cyclops() {
        let tree = scene::resolve(&scene::parse(
            "---\nscene: 1\nname: panel\ncitizen: test\n---\n```mix\nroot: {widget: \"row\", children: [\"launcher_icon\"]}\nlauncher_icon: {widget: \"image\", src: \"\", w: 28, h: 28}\n```\n",
        ).unwrap()).unwrap();
        let lists = crate::templates::validate_templates(&tree).unwrap();
        // Where share/brand installs it: icons/mixos/symbolic/apps/dev.mixos.svg.
        let root = std::env::temp_dir().join(format!("compd-launcher-{}", std::process::id()));
        let mark = root.join("icons/mixos/symbolic/apps/dev.mixos.svg");
        std::fs::create_dir_all(mark.parent().unwrap()).unwrap();
        std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/embedded-icon.svg"), &mark).unwrap();
        let images = prepare_with(&tree, &lists, &Mutex::new(AssetCache::new()), |name, _, _| {
            assert_eq!(name, "dev.mixos");
            Some(mark.clone())
        });
        assert!(matches!(images.get(&key("launcher_icon", None)), Some(Asset::Svg(_, false))), "the cyclops keeps its colours (its eye is red)");
        assert_eq!(fallback_image("launcher_text"), Some("launcher_icon"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn injected_resolvers_use_independent_asset_caches() {
        let tree = scene::resolve(&scene::parse(
            "---\nscene: 1\nname: icons\ncitizen: test\n---\n```mix\nroot: {widget: \"image\", src: \"same-source\", w: 31, h: 31}\n```\n",
        ).unwrap()).unwrap();
        let lists = crate::templates::validate_templates(&tree).unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let raster_cache = Mutex::new(AssetCache::new());
        let svg_cache = Mutex::new(AssetCache::new());
        let root = key("root", None);
        let rasters = prepare_with(&tree, &lists, &raster_cache, |_, _, _| Some(fixtures.join("icon.png")));
        let svgs = prepare_with(&tree, &lists, &svg_cache, |_, _, _| Some(fixtures.join("embedded-icon.svg")));
        assert!(matches!(rasters.get(&root), Some(Asset::Raster(Handle::Rgba { .. }))));
        assert!(matches!(svgs.get(&root), Some(Asset::Svg(_, _))));
        // Reusing a cache bypasses resolution and keeps the same decoded handle.
        let cached = prepare_with(&tree, &lists, &raster_cache, |_, _, _| panic!("cached asset must bypass lookup"));
        let (Some(Asset::Raster(first)), Some(Asset::Raster(second))) = (rasters.get(&root), cached.get(&root)) else {
            panic!("expected cached raster handles")
        };
        assert_eq!(first.id(), second.id());
    }

    #[test]
    fn rasters_are_ready_in_the_first_frame_and_reuse_their_handle() {
        // A real PNG decoder path, independent of GPU upload timing.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/icon.png");
        let cache = Mutex::new(RasterCache::new());
        let first = raster_with_cache(&path, &cache).unwrap();
        let second = raster_with_cache(&path, &cache).unwrap();
        assert_eq!(first.id(), second.id());
        let Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } = first
        else {
            panic!("file handle would defer loading")
        };
        assert_eq!((width, height, pixels.len()), (1, 1, 4));
        assert!(pixels[3] > 0);
        assert!(raster_with_cache(Path::new("/missing/scene-icon.png"), &cache).is_none());
    }

    #[test]
    fn raster_cache_pressure_preserves_live_handles_and_prunes_unused_entries() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/icon.png");
        // A distinct cache key for the same fixture; no temporary files needed.
        let other = path.parent().unwrap().join("../fixtures/icon.png");
        for keep_live in [false, true] {
            let cache = Mutex::new(RasterCache::new());
            let first = raster_with_cache(&path, &cache).unwrap();
            let mut live = Vec::new();
            {
                let mut entries = cache.lock().unwrap();
                for index in 0..511 {
                    let handle = Handle::from_rgba(1, 1, vec![0; 4]);
                    if keep_live { live.push(handle.clone()); }
                    entries.insert(PathBuf::from(format!("unused-{index}.png")), (None, 0, handle));
                }
                assert_eq!(entries.len(), 512);
            }
            let _other = raster_with_cache(&other, &cache).unwrap();
            let second = raster_with_cache(&path, &cache).unwrap();
            assert_eq!(first.id(), second.id(), "keep_live={keep_live}");
            assert_eq!(cache.lock().unwrap().len(), if keep_live { 513 } else { 2 });
            drop(live);
        }
    }

    #[test]
    fn concurrent_raster_lookups_share_one_handle() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/icon.png");
        let cache = Mutex::new(RasterCache::new());
        let start = std::sync::Barrier::new(8);
        let handles = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8).map(|_| scope.spawn(|| {
                start.wait();
                raster_with_cache(&path, &cache).unwrap()
            })).collect();
            threads.into_iter().map(|thread| thread.join().unwrap()).collect::<Vec<_>>()
        });
        assert!(handles.iter().all(|handle| handle.id() == handles[0].id()));
        assert_eq!(cache.lock().unwrap().len(), 1);
    }

    #[test]
    fn png_decode_produces_painted_rgba_pixels() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/icon.png");
        let Handle::Rgba { width, height, pixels, .. } = raster(&path).unwrap() else {
            panic!("PNG must be decoded before drawing")
        };
        assert_eq!((width, height, pixels.len()), (1, 1, 4));
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[0] > 0 && pixel[3] > 0), "blank decoded PNG");
    }

    #[test]
    fn svg_embedded_png_rasterises_to_non_transparent_pixels() {
        // Match iced_wgpu's CPU rasteriser, including raster-images support,
        // without the atlas upload or any adapter/device creation.
        let tree = resvg::usvg::Tree::from_data(
            include_bytes!("../tests/fixtures/embedded-icon.svg"),
            &resvg::usvg::Options::default(),
        ).unwrap();
        let size = tree.size().to_int_size();
        assert_eq!((size.width(), size.height()), (8, 8));
        let mut pixels = resvg::tiny_skia::Pixmap::new(size.width(), size.height()).unwrap();
        resvg::render(&tree, resvg::tiny_skia::Transform::default(), &mut pixels.as_mut());
        assert!(pixels.data().chunks_exact(4).any(|pixel| pixel[0] > 0 && pixel[3] > 0), "blank embedded PNG");
    }
}
