//! The `props.changed` cause per source.
//!
//! Whatever changes compd state notes a cause against the props subtree it
//! touched; the edge pass gives each changed path the cause of the most
//! specific noted prefix, and compd's generic `compd.state` only when nothing
//! was noted. The notes are cleared once the pass that diffs them has run.
//! The vocabulary: `wayland.map`, `wayland.unmap`, `wayland.focus`,
//! `comp.window`, `props.set`, `workspace.switch`, `layer.arrange`,
//! `output.geometry`, `session.lock`.

#[derive(Debug, Default)]
pub struct Causes {
    /// `(props path prefix, cause)`, newest last; `""` is the whole tree.
    entries: Vec<(String, &'static str)>,
}

impl Causes {
    /// `cause` changed something under `prefix` (newest wins on a tie).
    pub fn note(&mut self, prefix: impl Into<String>, cause: &'static str) {
        let prefix = prefix.into();
        self.entries.retain(|(entry, _)| *entry != prefix);
        self.entries.push((prefix, cause));
    }

    /// `cause` changed window `id`: its surfaces row and its windows row.
    pub fn note_window(&mut self, id: u64, cause: &'static str) {
        self.note(format!("surfaces.s{id}"), cause);
        self.note(format!("windows.s{id}"), cause);
    }

    /// The cause of the most specific noted prefix of `path`.
    pub fn resolve(&self, path: &str) -> Option<&'static str> {
        let under = |prefix: &str| {
            prefix.is_empty()
                || path == prefix
                || path.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('.'))
        };
        self.entries
            .iter()
            .rev()
            .filter(|(prefix, _)| under(prefix))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(_, cause)| *cause)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_most_specific_prefix_wins_and_the_tree_is_the_fallback() {
        let mut causes = Causes::default();
        causes.note("", "workspace.switch");
        causes.note_window(3, "wayland.map");
        causes.note("focus", "wayland.focus");
        assert_eq!(causes.resolve("windows.s3.visible"), Some("wayland.map"));
        assert_eq!(causes.resolve("surfaces.s3"), Some("wayland.map"));
        assert_eq!(causes.resolve("surfaces.s30.visible"), Some("workspace.switch"));
        assert_eq!(causes.resolve("focus.keyboard"), Some("wayland.focus"));
        causes.note_window(3, "comp.window");
        assert_eq!(causes.resolve("windows.s3.maximized"), Some("comp.window"));
        causes.clear();
        assert_eq!(causes.resolve("windows.s3.maximized"), None);
    }
}
