// SPDX-License-Identifier: MIT OR Apache-2.0
//! The file requester: [`Requester`], an Open / Save As file picker as
//! application state over a [`Filesystem`] trait — `std::fs` by default
//! ([`StdFs`]), a fake in tests, no GTK, no rfd, no portals.
//!
//! The field accepts an absolute path, `~/…`, or a name relative to the
//! listed directory, with Tab completion. Submitting a directory lists
//! it; submitting a file resolves [`Outcome::Open`] (or
//! [`Outcome::Save`] with an `exists` flag for Save As, so the app can
//! confirm an overwrite). The listing is directories first, capped at
//! [`MAX_ENTRIES`], with a hidden-files toggle and a recents row.
//!
//! The view ([`Requester::view`]) is built from toolkit's own widgets
//! and theme; mount it in a [`dialog::modal`](crate::dialog) for the
//! scrim and focus trap.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced_core::{Element, Length, Padding};
use iced_widget::text_input;
use iced_widget::{button, column, container, row, scrollable, text};

use crate::theme::{self, Theme};
use crate::tokens::Tokens;

/// The widget id of the requester's path field, so the app can focus it
/// on open.
pub const PATH_INPUT: &str = "toolkit-requester-path";

/// Directory entries listed at most (a huge directory stays responsive).
pub const MAX_ENTRIES: usize = 2000;

/// What the requester reads from and writes to. `std::fs` by default;
/// tests and sandboxes substitute their own.
pub trait Filesystem {
    /// One listed entry.
    fn list(&self, dir: &Path, hidden: bool) -> std::io::Result<(Vec<Entry>, bool)>;

    /// Whether `path` is a directory (following links).
    fn is_dir(&self, path: &Path) -> bool;

    /// Whether `path` exists.
    fn exists(&self, path: &Path) -> bool;

    /// The user's home directory, for `~` resolution.
    fn home(&self) -> Option<PathBuf> {
        None
    }
}

/// The default [`Filesystem`]: `std::fs`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdFs;

impl Filesystem for StdFs {
    fn list(&self, dir: &Path, hidden: bool) -> std::io::Result<(Vec<Entry>, bool)> {
        list(dir, hidden)
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn home(&self) -> Option<PathBuf> {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    }
}

/// A shared filesystem handle, so the state and the view agree on one.
pub type Fs = Arc<dyn Filesystem + Send + Sync>;

/// A convenience: an [`Fs`] over [`StdFs`].
#[must_use]
pub fn std_fs() -> Fs {
    Arc::new(StdFs)
}

/// One listed entry: a name and whether it is a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The file name.
    pub name: String,
    /// Whether the entry is a directory (links followed).
    pub dir: bool,
}

/// What the requester is being asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Pick a file (or several, app-side) to open.
    Open,
    /// Pick where to save; an existing target is reported so the app can
    /// confirm the overwrite.
    Save,
}

/// What the user did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The path field changed.
    Input(String),
    /// Tab was pressed in the path field (completion requested).
    Complete,
    /// The up arrow.
    Up,
    /// The down arrow.
    Down,
    /// Enter in the path field, or the primary button.
    Submit,
    /// A row was clicked.
    Select(usize),
    /// A row was double-clicked.
    Activate(usize),
    /// The parent-directory button.
    Parent,
    /// The hidden-files toggle.
    ToggleHidden,
    /// A recent path was clicked.
    Recent(String),
}

/// What submitting produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Open these paths (one per pick; the dialog stays for multi-pick
    /// app shells).
    Open(Vec<String>),
    /// Save to this path; `exists` says whether it is already there.
    Save { path: String, exists: bool },
}

/// The requester's state.
#[derive(Clone)]
pub struct Requester {
    mode: Mode,
    dir: PathBuf,
    input: String,
    entries: Vec<Entry>,
    truncated: bool,
    hidden: bool,
    selected: Option<usize>,
    recent: Vec<String>,
    error: Option<String>,
    fs: Fs,
}

/// Lists `dir` over `std::fs`: directories first, then files, each by
/// case-insensitive name, dot-files only when `hidden`.
pub fn list(dir: &Path, hidden: bool) -> std::io::Result<(Vec<Entry>, bool)> {
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !hidden && name.starts_with('.') {
                return None;
            }
            // Follow symlinks: a link to a directory lists as a directory.
            let dir = std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir());
            Some(Entry { name, dir })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let truncated = entries.len() > MAX_ENTRIES;
    entries.truncate(MAX_ENTRIES);
    Ok((entries, truncated))
}

/// Resolves what the user typed against the listed directory: `~`,
/// `~/…`, an absolute path, or a name relative to `dir`.
#[must_use]
pub fn resolve(dir: &Path, input: &str, home: Option<&Path>) -> PathBuf {
    let input = input.trim();
    if input == "~" {
        return home.map_or_else(|| dir.to_path_buf(), Path::to_path_buf);
    }
    if let Some(rest) = input.strip_prefix("~/")
        && let Some(home) = home
    {
        return home.join(rest);
    }
    let p = Path::new(input);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        dir.join(p)
    }
}

/// Tab completion over a [`Filesystem`]: the longest common prefix of
/// the names in the typed path's directory that start with its last
/// component; a unique directory match gets a trailing `/`. `None` when
/// nothing matches or nothing would change.
#[must_use]
pub fn complete(fs: &dyn Filesystem, dir: &Path, input: &str, hidden: bool) -> Option<String> {
    let (head, stem) = match input.rfind('/') {
        Some(i) => (&input[..=i], &input[i + 1..]),
        None => ("", input),
    };
    let base = if head.is_empty() {
        dir.to_path_buf()
    } else {
        resolve(dir, head, fs.home().as_deref())
    };
    let (entries, _) = fs.list(&base, hidden || stem.starts_with('.')).ok()?;
    let matches: Vec<&Entry> = entries
        .iter()
        .filter(|e| e.name.starts_with(stem))
        .collect();
    let first = matches.first()?;
    let mut prefix = first.name.clone();
    for m in &matches[1..] {
        let common = prefix
            .chars()
            .zip(m.name.chars())
            .take_while(|(a, b)| a == b)
            .count();
        prefix = prefix.chars().take(common).collect();
    }
    let mut out = format!("{head}{prefix}");
    if matches.len() == 1 && first.dir {
        out.push('/');
    }
    (out != input).then_some(out)
}

impl Requester {
    /// A requester in `mode` starting at `dir`, reading `fs`, offering
    /// `recent` paths (Open only). A Save requester's field starts as
    /// "untitled.txt".
    #[must_use]
    pub fn new(mode: Mode, dir: PathBuf, recent: Vec<String>, fs: Fs) -> Self {
        let mut r = Requester {
            mode,
            dir,
            input: String::new(),
            entries: Vec::new(),
            truncated: false,
            hidden: false,
            selected: None,
            recent,
            error: None,
            fs,
        };
        if matches!(r.mode, Mode::Save) {
            r.input = "untitled.txt".into();
        }
        r.relist();
        r
    }

    /// Prefills the name (Save As of a named buffer).
    #[must_use]
    pub fn with_name(mut self, name: &str) -> Self {
        self.input = name.to_owned();
        self
    }

    /// The listed directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The current field text.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }

    /// The dialog title for the mode.
    #[must_use]
    pub fn title(&self) -> &'static str {
        match self.mode {
            Mode::Open => "Open File",
            Mode::Save => "Save As",
        }
    }

    fn relist(&mut self) {
        match self.fs.list(&self.dir, self.hidden) {
            Ok((entries, truncated)) => {
                self.entries = entries;
                self.truncated = truncated;
                self.error = None;
            }
            Err(e) => {
                self.entries.clear();
                self.error = Some(format!("{}: {e}", self.dir.display()));
            }
        }
        self.selected = None;
    }

    fn navigate(&mut self, dir: PathBuf) {
        self.dir = dir;
        if matches!(self.mode, Mode::Open) {
            self.input.clear();
        }
        self.relist();
    }

    /// Advances the state; the outcome of a completed pick, if any.
    pub fn update(&mut self, event: Event) -> Option<Outcome> {
        match event {
            Event::Input(s) => {
                self.input = s;
                self.error = None;
            }
            Event::Complete => {
                if let Some(c) = complete(self.fs.as_ref(), &self.dir, &self.input, self.hidden) {
                    self.input = c;
                }
            }
            Event::Up => {
                self.selected = match self.selected {
                    None | Some(0) => (!self.entries.is_empty()).then_some(0),
                    Some(i) => Some(i - 1),
                };
                self.take_selection();
            }
            Event::Down => {
                let last = self.entries.len().checked_sub(1)?;
                self.selected = Some(self.selected.map_or(0, |i| (i + 1).min(last)));
                self.take_selection();
            }
            Event::Select(i) => {
                self.selected = Some(i);
                self.take_selection();
            }
            Event::Activate(i) => {
                self.selected = Some(i);
                self.take_selection();
                return self.submit();
            }
            Event::Parent => {
                if let Some(parent) = self.dir.parent() {
                    let parent = parent.to_path_buf();
                    self.navigate(parent);
                }
            }
            Event::ToggleHidden => {
                self.hidden = !self.hidden;
                self.relist();
            }
            Event::Recent(path) => {
                self.input = path;
                return self.submit();
            }
            Event::Submit => return self.submit(),
        }
        None
    }

    fn take_selection(&mut self) {
        if let Some(e) = self.selected.and_then(|i| self.entries.get(i)) {
            self.input = if e.dir {
                format!("{}/", e.name)
            } else {
                e.name.clone()
            };
        }
    }

    fn submit(&mut self) -> Option<Outcome> {
        if self.input.trim().is_empty() {
            return None;
        }
        let path = resolve(&self.dir, &self.input, self.fs.home().as_deref());
        if self.fs.is_dir(&path) {
            self.navigate(path);
            return None;
        }
        let text = path.to_string_lossy().into_owned();
        match self.mode {
            Mode::Open => Some(Outcome::Open(vec![text])),
            Mode::Save => {
                if !path.parent().is_some_and(|p| self.fs.is_dir(p)) {
                    self.error = Some(format!(
                        "{} does not exist",
                        path.parent().unwrap_or(Path::new("/")).display()
                    ));
                    return None;
                }
                Some(Outcome::Save {
                    path: text,
                    exists: self.fs.exists(&path),
                })
            }
        }
    }

    /// The requester's body, from the theme, producing `Message`s from
    /// [`Event`] (fold them into the app's enum with a `From` impl).
    /// The strings are the app's ([`Strings`]).
    pub fn view<'a, Message>(
        &'a self,
        tokens: Tokens,
        strings: &'a Strings,
    ) -> Element<'a, Message, Theme, iced_widget::Renderer>
    where
        Message: From<Event> + Clone + 'a,
    {
        self.view_inner::<Message, iced_widget::Renderer>(tokens, strings)
    }

    fn view_inner<'a, Message, Renderer>(
        &'a self,
        tokens: Tokens,
        strings: &'a Strings,
    ) -> Element<'a, Message, Theme, Renderer>
    where
        Message: From<Event> + Clone + 'a,
        Renderer: iced_core::text::Renderer + 'static,
        Theme: text_input::Catalog
            + iced_widget::button::Catalog
            + iced_core::widget::text::Catalog
            + iced_widget::scrollable::Catalog
            + iced_widget::container::Catalog
            + 'a,
        <Theme as iced_widget::button::Catalog>::Class<'a>:
            From<iced_widget::button::StyleFn<'a, Theme>>,
        <Theme as iced_widget::container::Catalog>::Class<'a>:
            From<iced_widget::container::StyleFn<'a, Theme>>,
    {
        let t = tokens.palette;
        let m = tokens.metrics;
        let gap = m.spacing.sm;
        let pad = Padding::from([6.0, 10.0]);

        let field = text_input(strings.placeholder.as_str(), &self.input)
            .id(iced_core::widget::Id::new(PATH_INPUT))
            .on_input(|s| Message::from(Event::Input(s)))
            .on_submit(Message::from(Event::Submit))
            .size(m.text.md)
            .padding(pad);

        let location = row![
            button(text("↑").size(m.text.md))
                .on_press(Message::from(Event::Parent))
                .style(theme::button::secondary),
            text(self.dir.to_string_lossy().to_string())
                .size(m.text.sm)
                .width(Length::Fill),
            button(text(if self.hidden {
                strings.hide_hidden.clone()
            } else {
                strings.show_hidden.clone()
            }))
            .on_press(Message::from(Event::ToggleHidden))
            .style(theme::button::text),
        ]
        .spacing(gap)
        .align_y(iced_core::alignment::Vertical::Center);

        let mut list = column![].spacing(0);
        for (i, e) in self.entries.iter().enumerate() {
            let selected = self.selected == Some(i);
            let label = if e.dir {
                format!("{}/", e.name)
            } else {
                e.name.clone()
            };
            let fg = if selected {
                t.selection_text
            } else if e.dir {
                t.elevated_text
            } else {
                t.text
            };
            let item = button(text(label).size(m.text.sm).color(fg))
                .width(Length::Fill)
                .padding(Padding::from([2.0, 8.0]))
                .on_press(Message::from(Event::Select(i)))
                .style(move |_, status| iced_widget::button::Style {
                    background: if selected {
                        Some(t.selection.into())
                    } else if matches!(status, button::Status::Hovered) {
                        Some(t.muted_surface.into())
                    } else {
                        None
                    },
                    text_color: fg,
                    border: iced_core::Border {
                        radius: m.radius.sm.into(),
                        ..iced_core::Border::default()
                    },
                    ..iced_widget::button::Style::default()
                });
            list = list.push(item);
        }
        if self.truncated {
            list = list.push(
                text(strings.truncated.clone())
                    .size(m.text.sm)
                    .color(t.muted_text),
            );
        }

        let listing = container(scrollable(list).height(Length::Fixed(260.0)))
            .padding(4)
            .style(move |_| container::Style {
                background: Some(t.surface.into()),
                border: iced_core::Border {
                    color: t.border,
                    width: m.border.width,
                    radius: m.radius.sm.into(),
                },
                ..container::Style::default()
            });

        let mut body = column![location, listing, field].spacing(gap);

        if !self.recent.is_empty() && matches!(self.mode, Mode::Open) {
            let mut recent = row![
                text(strings.recent.clone())
                    .size(m.text.sm)
                    .color(t.muted_text)
            ]
            .spacing(gap)
            .align_y(iced_core::alignment::Vertical::Center);
            for path in self.recent.iter().take(6) {
                let name = Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone());
                let msg = Message::from(Event::Recent(path.clone()));
                recent = recent.push(
                    button(text(name).size(m.text.sm))
                        .on_press(msg)
                        .style(theme::button::text),
                );
            }
            body = body.push(
                scrollable(recent).direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::new().width(3).scroller_width(3),
                )),
            );
        }
        if let Some(error) = &self.error {
            body = body.push(text(error.as_str()).size(m.text.sm).color(t.destructive));
        }

        crate::keys::keys(body, |_| None)
            .on_key_before(|event| {
                use iced_core::keyboard::{Event as KeyEvent, Key, key::Named};
                let KeyEvent::KeyPressed { key, modifiers, .. } = event else {
                    return None;
                };
                if !modifiers.is_empty() {
                    return None;
                }
                match key {
                    Key::Named(Named::Tab) => Some(Message::from(Event::Complete)),
                    Key::Named(Named::ArrowUp) => Some(Message::from(Event::Up)),
                    Key::Named(Named::ArrowDown) => Some(Message::from(Event::Down)),
                    _ => None,
                }
            })
            .into()
    }
}

/// The requester's localised strings. English defaults exist
/// ([`Strings::english`]).
#[derive(Debug, Clone, Default)]
pub struct Strings {
    /// The path field's placeholder.
    pub placeholder: String,
    /// The parent-directory button's tooltip text.
    pub show_hidden: String,
    /// The hidden-files toggle once hidden files show.
    pub hide_hidden: String,
    /// The truncation notice.
    pub truncated: String,
    /// The recents row's label.
    pub recent: String,
}

impl Strings {
    /// The English strings.
    #[must_use]
    pub fn english() -> Self {
        Self {
            placeholder: "File name or path (Tab completes)".into(),
            show_hidden: "Show Hidden".into(),
            hide_hidden: "Hide Hidden".into(),
            truncated: format!("… more than {MAX_ENTRIES} entries; type to narrow"),
            recent: "Recent:".into(),
        }
    }
}

/// The message the requester's [`view`](Requester::view) produces; use
/// it directly, or fold [`Event`] into the app's own enum with a
/// `From` impl and pass that type instead.
#[derive(Debug, Clone)]
pub struct ViewMessage(pub Event);

impl From<Event> for ViewMessage {
    fn from(event: Event) -> Self {
        Self(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn tree(dir: &Path) {
        std::fs::create_dir(dir.join("src")).unwrap();
        std::fs::create_dir(dir.join("scenes")).unwrap();
        std::fs::write(dir.join("scene.mix"), "").unwrap();
        std::fs::write(dir.join("Readme.md"), "").unwrap();
        std::fs::write(dir.join(".hidden"), "").unwrap();
    }

    #[test]
    fn listing_puts_directories_first_and_hides_dotfiles() {
        let d = tempfile::tempdir().unwrap();
        tree(d.path());
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
        let d = tempfile::tempdir().unwrap();
        tree(d.path());
        let fs = StdFs;
        assert_eq!(
            complete(&fs, d.path(), "sr", false).as_deref(),
            Some("src/"),
            "a unique directory gets its slash"
        );
        assert_eq!(
            complete(&fs, d.path(), "sc", false).as_deref(),
            Some("scene"),
            "common prefix of scene.mix and scenes"
        );
        assert_eq!(complete(&fs, d.path(), "scene", false), None);
        assert_eq!(complete(&fs, d.path(), "zz", false), None);
        assert_eq!(
            complete(&fs, d.path(), ".h", false).as_deref(),
            Some(".hidden"),
            "a dot prefix completes hidden names"
        );
        let abs = format!("{}/Rea", d.path().display());
        assert_eq!(
            complete(&fs, Path::new("/"), &abs, false),
            Some(format!("{}/Readme.md", d.path().display()))
        );
    }

    #[test]
    fn resolve_tilde_and_relative() {
        let home = Path::new("/home/user");
        assert_eq!(
            resolve(Path::new("/tmp"), "~", Some(home),),
            PathBuf::from("/home/user")
        );
        assert_eq!(
            resolve(Path::new("/tmp"), "~/docs/a.md", Some(home)),
            PathBuf::from("/home/user/docs/a.md")
        );
        assert_eq!(
            resolve(Path::new("/tmp"), "/etc/passwd", Some(home)),
            PathBuf::from("/etc/passwd")
        );
        assert_eq!(
            resolve(Path::new("/tmp"), "name.txt", Some(home)),
            PathBuf::from("/tmp/name.txt")
        );
    }

    #[test]
    fn submitting_navigates_directories_and_opens_files() {
        let d = tempfile::tempdir().unwrap();
        tree(d.path());
        let mut dlg = Requester::new(Mode::Open, d.path().to_path_buf(), Vec::new(), std_fs());
        assert_eq!(dlg.update(Event::Input("src".into())), None);
        assert_eq!(dlg.update(Event::Submit), None);
        assert_eq!(dlg.dir(), d.path().join("src"));
        dlg.update(Event::Parent);
        dlg.update(Event::Down);
        dlg.update(Event::Down);
        dlg.update(Event::Down);
        assert_eq!(dlg.input(), "Readme.md");
        let out = dlg.update(Event::Submit);
        assert_eq!(
            out,
            Some(Outcome::Open(vec![
                d.path().join("Readme.md").to_string_lossy().into_owned()
            ]))
        );
    }

    #[test]
    fn save_reports_existing_targets() {
        let d = tempfile::tempdir().unwrap();
        tree(d.path());
        let mut dlg = Requester::new(Mode::Save, d.path().to_path_buf(), Vec::new(), std_fs())
            .with_name("scene.mix");
        match dlg.update(Event::Submit) {
            Some(Outcome::Save {
                exists: true, path, ..
            }) => {
                assert!(path.ends_with("scene.mix"))
            }
            other => panic!("{other:?}"),
        }
        dlg.update(Event::Input("nope/x.txt".into()));
        assert_eq!(dlg.update(Event::Submit), None);
        assert!(
            dlg.error.as_ref().is_some_and(|e| !e.is_empty()),
            "a missing parent is refused"
        );
    }

    /// A filesystem that never touches the disk: the listing is fixed.
    struct FakeFs {
        home: PathBuf,
        calls: Mutex<Vec<String>>,
    }

    impl FakeFs {
        fn new(home: PathBuf) -> Self {
            Self {
                home,
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl Filesystem for FakeFs {
        fn list(&self, dir: &Path, hidden: bool) -> std::io::Result<(Vec<Entry>, bool)> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{}:{hidden}", dir.display()));
            Ok((
                vec![
                    Entry {
                        name: "docs".into(),
                        dir: true,
                    },
                    Entry {
                        name: "notes.txt".into(),
                        dir: false,
                    },
                ],
                false,
            ))
        }

        fn is_dir(&self, path: &Path) -> bool {
            path.file_name().is_some_and(|n| n == "docs")
        }

        fn exists(&self, _path: &Path) -> bool {
            false
        }

        fn home(&self) -> Option<PathBuf> {
            Some(self.home.clone())
        }
    }

    #[test]
    fn a_fake_filesystem_drives_the_state_machine() {
        let fake = Arc::new(FakeFs::new(PathBuf::from("/fake/home")));
        let fs: Fs = fake.clone();
        let mut dlg = Requester::new(Mode::Open, PathBuf::from("/start"), Vec::new(), fs);
        assert_eq!(dlg.dir(), Path::new("/start"));
        // Navigating into the fake's only directory.
        dlg.update(Event::Input("docs".into()));
        dlg.update(Event::Submit);
        assert_eq!(dlg.dir(), Path::new("/start/docs"));
        // ~ resolves through the fake's home.
        dlg.update(Event::Input("~/x.md".into()));
        assert_eq!(
            dlg.update(Event::Submit),
            Some(Outcome::Open(vec!["/fake/home/x.md".into()]))
        );
        assert!(
            fake.calls
                .lock()
                .unwrap()
                .iter()
                .all(|c| c.starts_with("/start")),
            "the fake never touches a real path: {:?}",
            fake.calls.lock().unwrap()
        );
    }
}
