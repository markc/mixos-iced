// SPDX-License-Identifier: MIT OR Apache-2.0
//! Save intent and tab ownership around the shared Open/Save requester.
use super::{DialogMsg, frame};
use crate::app::Msg;
use crate::chrome::Look;
use application::iced::Element;
use editor_model::types::{Intent, TabId};
#[cfg(test)]
use requester::list;
pub use requester::{Event as FileMsg, PATH_INPUT};
use std::path::{Path, PathBuf};
use toolkit::requester::{self, Requester};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileMode {
    Open,
    SaveAs { tab: TabId, intent: Intent },
}
#[derive(Clone)]
pub struct FileDialog {
    pub mode: FileMode,
    requester: Requester,
    strings: requester::Strings,
}
impl std::fmt::Debug for FileDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileDialog")
            .field("mode", &self.mode)
            .field("dir", &self.requester.dir())
            .field("input", &self.requester.input())
            .finish()
    }
}
impl std::ops::Deref for FileDialog {
    type Target = Requester;
    fn deref(&self) -> &Requester {
        &self.requester
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOutcome {
    Open(Vec<String>),
    SaveAs {
        tab: TabId,
        intent: Intent,
        path: String,
        exists: bool,
    },
}
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}
#[cfg(test)]
fn complete(dir: &Path, input: &str, hidden: bool) -> Option<String> {
    requester::complete(&requester::StdFs, dir, input, hidden)
}
impl FileDialog {
    pub fn new(mode: FileMode, dir: PathBuf, recent: Vec<String>) -> Self {
        let kind = match mode {
            FileMode::Open => requester::Mode::Open,
            FileMode::SaveAs { .. } => requester::Mode::Save,
        };
        Self {
            mode,
            requester: Requester::new(kind, dir, recent, requester::std_fs()),
            strings: requester::Strings::english(),
        }
    }
    pub fn with_name(mut self, name: &str) -> Self {
        self.requester = self.requester.with_name(name);
        self
    }
    pub fn update(&mut self, message: FileMsg) -> Option<FileOutcome> {
        match self.requester.update(message)? {
            requester::Outcome::Open(paths) => Some(FileOutcome::Open(paths)),
            requester::Outcome::Save { path, exists } => match &self.mode {
                FileMode::SaveAs { tab, intent } => Some(FileOutcome::SaveAs {
                    tab: *tab,
                    intent: intent.clone(),
                    path,
                    exists,
                }),
                FileMode::Open => None,
            },
        }
    }
    pub fn view<'a>(&'a self, look: Look) -> Element<'a, Msg> {
        let body = self
            .requester
            .view_for::<FileMsg, application::iced::Theme, application::cpu::Renderer>(
                look.tokens,
                &self.strings,
            )
            .map(file_message);
        let action = match self.mode {
            FileMode::Open => "Open",
            FileMode::SaveAs { .. } => "Save",
        };
        frame(
            look,
            self.requester.title(),
            body,
            vec![
                look.button("Cancel", Some(Msg::Dialog(DialogMsg::Close)))
                    .style(look.secondary())
                    .into(),
                look.button(
                    action,
                    (!self.requester.input().trim().is_empty())
                        .then_some(Msg::Dialog(DialogMsg::File(FileMsg::Submit))),
                )
                .style(look.primary())
                .into(),
            ],
            640.0,
        )
    }
}

fn file_message(event: FileMsg) -> Msg {
    match event {
        FileMsg::Complete => Msg::FileTab,
        event => Msg::Dialog(DialogMsg::File(event)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_uses_the_cursor_positioning_handler() {
        assert!(matches!(file_message(FileMsg::Complete), Msg::FileTab));
        assert!(matches!(
            file_message(FileMsg::Input("src/".into())),
            Msg::Dialog(DialogMsg::File(FileMsg::Input(_)))
        ));
    }

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("src")).unwrap();
        std::fs::create_dir(d.path().join("scenes")).unwrap();
        std::fs::write(d.path().join("scene.mix"), "").unwrap();
        std::fs::write(d.path().join("Readme.md"), "").unwrap();
        std::fs::write(d.path().join(".hidden"), "").unwrap();
        d
    }

    #[test]
    fn listing_puts_directories_first_and_hides_dotfiles() {
        let d = tree();
        let (entries, truncated) = list(d.path(), false).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["scenes", "src", "Readme.md", "scene.mix"]);
        assert!(!truncated);
        assert!(
            list(d.path(), true)
                .unwrap()
                .0
                .iter()
                .any(|e| e.name == ".hidden")
        );
    }

    #[test]
    fn tab_completion() {
        let d = tree();
        assert_eq!(
            complete(d.path(), "sr", false).as_deref(),
            Some("src/"),
            "a unique directory gets its slash"
        );
        assert_eq!(
            complete(d.path(), "sc", false).as_deref(),
            Some("scene"),
            "common prefix of scene.mix and scenes"
        );
        assert_eq!(
            complete(d.path(), "scene", false),
            None,
            "nothing more to add"
        );
        assert_eq!(complete(d.path(), "zz", false), None);
        assert_eq!(
            complete(d.path(), ".h", false).as_deref(),
            Some(".hidden"),
            "a dot prefix completes hidden names"
        );
        let abs = format!("{}/Rea", d.path().display());
        assert_eq!(
            complete(Path::new("/"), &abs, false),
            Some(format!("{}/Readme.md", d.path().display()))
        );
    }

    #[test]
    fn submitting_navigates_directories_and_opens_files() {
        let d = tree();
        let mut dlg = FileDialog::new(FileMode::Open, d.path().to_path_buf(), Vec::new());
        assert_eq!(dlg.update(FileMsg::Input("src".into())), None);
        assert_eq!(dlg.update(FileMsg::Submit), None);
        assert_eq!(dlg.dir(), d.path().join("src"));
        dlg.update(FileMsg::Parent);
        dlg.update(FileMsg::Down);
        dlg.update(FileMsg::Down);
        dlg.update(FileMsg::Down);
        assert_eq!(dlg.input(), "Readme.md");
        let out = dlg.update(FileMsg::Submit);
        assert_eq!(
            out,
            Some(FileOutcome::Open(vec![
                d.path().join("Readme.md").to_string_lossy().into_owned()
            ]))
        );
    }

    #[test]
    fn save_as_reports_existing_targets() {
        let d = tree();
        let intent = Intent::ui(4);
        let mut dlg = FileDialog::new(
            FileMode::SaveAs {
                tab: 4,
                intent: intent.clone(),
            },
            d.path().to_path_buf(),
            Vec::new(),
        )
        .with_name("scene.mix");
        match dlg.update(FileMsg::Submit) {
            Some(FileOutcome::SaveAs {
                tab: 4,
                exists: true,
                path,
                ..
            }) => assert!(path.ends_with("scene.mix")),
            other => panic!("{other:?}"),
        }
        dlg.update(FileMsg::Input("nope/x.txt".into()));
        assert_eq!(dlg.update(FileMsg::Submit), None);
        assert!(
            dlg.error().is_some(),
            "a missing parent is refused in the dialog"
        );
    }
}
