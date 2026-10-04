//! Local freedesktop icon lookup for scene images, over toolkit's generic
//! resolver: the apps service usually supplies a path; names and missing
//! paths also need a useful icon. iced loads SVG/PNG itself, at the
//! surface's output scale. The lookup follows the XDG environment of the
//! compositor process.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use toolkit::icons::freedesktop::{Lookup, Resolver};

pub(crate) fn is_svg(path: &Path) -> bool {
    toolkit::icons::freedesktop::is_svg(path)
}

fn resolver() -> &'static Resolver {
    static RESOLVER: OnceLock<Resolver> = OnceLock::new();
    RESOLVER.get_or_init(|| Resolver::new(Lookup::from_xdg()))
}

pub(crate) fn resolve(source: &str, size: u32, app: Option<&str>) -> Option<PathBuf> {
    resolver().resolve(source, size, app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolver_follows_the_process_environment() {
        let lookup = resolver().lookup();
        assert!(!lookup.theme.is_empty());
        assert!(!lookup.roots.is_empty());
        assert!(is_svg(Path::new("icon.SVG")));
    }

    #[test]
    #[ignore = "requires COMPD_ICON_ROOT pointing to an installed icon tree"]
    fn installed_icons_resolve_without_generic_placeholders() {
        let root = PathBuf::from(std::env::var_os("COMPD_ICON_ROOT").expect("COMPD_ICON_ROOT"));
        let lookup = Lookup::in_data_dirs(
            "Adwaita",
            [root.join("usr/local/share"), root.join("usr/share")],
        );
        for id in ["bssh", "bvnc", "avahi-discover", "dev.mixos.media"] {
            let path = lookup
                .resolve("", 32, Some(id))
                .unwrap_or_else(|| panic!("missing {id}"));
            let stem = path.file_stem().unwrap().to_string_lossy();
            assert!(
                !matches!(
                    stem.as_ref(),
                    "application-x-executable" | "application-default-icon"
                ),
                "{id}: {}",
                path.display()
            );
            assert!(path.is_file());
            println!("{id}: {}", path.display());
        }
    }
}
