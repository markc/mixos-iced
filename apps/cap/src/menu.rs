// SPDX-License-Identifier: MIT OR Apache-2.0
//! Cap's application menus. The app owns commands; toolkit owns navigation.
use crate::{
    app::Tool,
    capture::{Mode, Request, Window},
    document::{Document, Kind},
    strings::label,
};
use iced::keyboard::{Key, Modifiers, key::Named};
use toolkit::menu::Item;

pub const BAR_ID: &str = "cap-menu-bar";

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Open,
    Save,
    Copy,
    Quit,
    Take,
    Cancel,
    Refresh,
    Mode(Mode),
    Output(String),
    Window(Window),
    Delay(u32),
    Pointer(bool),
    Tool(Tool),
    Properties,
    Undo,
    Redo,
    Delete,
    Uncrop,
    ZoomIn,
    ZoomOut,
    Fit,
    Shortcuts,
    About,
}

pub struct Context<'a> {
    pub document: Option<&'a Document>,
    pub selected: Option<u64>,
    pub request: &'a Request,
    pub outputs: &'a [String],
    pub windows: &'a [Window],
    pub window: Option<&'a Window>,
    pub tool: Tool,
    pub busy: bool,
    pub capturing: bool,
    pub modal: bool,
}

pub fn enabled(action: &Action, c: &Context<'_>) -> bool {
    if c.modal {
        return false;
    }
    let editable = !c.busy && c.document.is_some();
    match action {
        Action::Quit => true,
        Action::Cancel => c.capturing,
        Action::Open | Action::Refresh | Action::Mode(_) | Action::Delay(_) => !c.busy,
        Action::Take => !c.busy && (c.request.mode != Mode::Window || c.window.is_some()),
        Action::Output(name) => {
            !c.busy && c.request.mode != Mode::Window && c.outputs.contains(name)
        }
        Action::Window(window) => {
            !c.busy && c.request.mode == Mode::Window && c.windows.contains(window)
        }
        Action::Pointer(_) => !c.busy && c.request.mode != Mode::Window,
        Action::Undo => editable && c.document.is_some_and(Document::can_undo),
        Action::Redo => editable && c.document.is_some_and(Document::can_redo),
        Action::Delete => {
            editable
                && c.document
                    .is_some_and(|d| d.objects().iter().any(|o| Some(o.id) == c.selected))
        }
        Action::Uncrop => editable && c.document.is_some_and(|d| d.crop().is_some()),
        Action::Save | Action::Copy | Action::Tool(_) | Action::Properties => editable,
        Action::ZoomIn | Action::ZoomOut | Action::Fit => c.document.is_some() && !c.busy,
        Action::Shortcuts | Action::About => !c.busy,
    }
}

fn entry(key: &str, action: Action, c: &Context<'_>) -> Item<Action> {
    let item = Item::action(label(key), action.clone()).enabled(enabled(&action, c));
    match accelerator(&action) {
        Some(chord) => item.accelerator(chord),
        None => item,
    }
}

fn checked(key: &str, action: Action, on: bool, c: &Context<'_>) -> Item<Action> {
    checked_label(label(key), action, on, c)
}

fn checked_label(text: String, action: Action, on: bool, c: &Context<'_>) -> Item<Action> {
    Item::action(
        format!("{} {text}", if on { "✓" } else { "\u{2007}" }),
        action.clone(),
    )
    .enabled(enabled(&action, c))
}

pub fn bar(c: &Context<'_>) -> Vec<Item<Action>> {
    use Action::*;
    let mut capture = vec![
        entry("take", Take, c),
        entry("cancel", Cancel, c),
        Item::separator(),
    ];
    capture.push(Item::submenu(
        label("capture-mode"),
        crate::capture::Mode::ALL
            .into_iter()
            .map(|mode| checked(mode.key(), Mode(mode), c.request.mode == mode, c))
            .collect(),
    ));
    let outputs = c
        .outputs
        .iter()
        .map(|name| {
            checked_label(
                name.clone(),
                Output(name.clone()),
                c.request.output.as_ref() == Some(name),
                c,
            )
        })
        .collect();
    capture.push(Item::submenu(label("output"), outputs).enabled(
        !c.busy && c.request.mode != crate::capture::Mode::Window && !c.outputs.is_empty(),
    ));
    let windows = c
        .windows
        .iter()
        .map(|window| {
            checked_label(
                window.to_string(),
                Window(window.clone()),
                c.window.is_some_and(|old| old.target == window.target),
                c,
            )
        })
        .collect();
    capture.push(Item::submenu(label("choose-window"), windows).enabled(
        !c.busy && c.request.mode == crate::capture::Mode::Window && !c.windows.is_empty(),
    ));
    capture.push(
        Item::submenu(
            label("delay"),
            (0..=10)
                .map(|delay| {
                    checked_label(delay.to_string(), Delay(delay), c.request.delay == delay, c)
                })
                .collect(),
        )
        .enabled(!c.busy),
    );
    capture.push(checked(
        "pointer",
        Pointer(!c.request.cursor),
        c.request.cursor,
        c,
    ));
    capture.push(Item::separator());
    capture.push(entry("refresh", Refresh, c));

    let mut annotate = vec![
        checked(
            "select",
            Tool(crate::app::Tool::Select),
            c.tool == crate::app::Tool::Select,
            c,
        ),
        checked(
            "crop",
            Tool(crate::app::Tool::Crop),
            c.tool == crate::app::Tool::Crop,
            c,
        ),
        Item::separator(),
    ];
    for kind in Kind::ALL {
        annotate.push(checked(
            kind.key(),
            Tool(crate::app::Tool::Draw(kind)),
            c.tool == crate::app::Tool::Draw(kind),
            c,
        ));
    }
    annotate.extend([
        Item::separator(),
        entry("annotation-properties", Properties, c),
    ]);
    vec![
        Item::submenu(
            label("menu-file"),
            vec![
                entry("open", Open, c),
                entry("save", Save, c),
                Item::separator(),
                entry("quit", Quit, c),
            ],
        ),
        Item::submenu(
            label("menu-edit"),
            vec![
                entry("undo", Undo, c),
                entry("redo", Redo, c),
                Item::separator(),
                entry("copy", Copy, c),
                entry("delete", Delete, c),
                Item::separator(),
                entry("uncrop", Uncrop, c),
            ],
        ),
        Item::submenu(label("menu-capture"), capture),
        Item::submenu(label("menu-annotate"), annotate),
        Item::submenu(
            label("menu-view"),
            vec![
                entry("zoom-in", ZoomIn, c),
                entry("zoom-out", ZoomOut, c),
                Item::separator(),
                entry("fit", Fit, c),
            ],
        ),
        Item::submenu(
            label("menu-help"),
            vec![
                entry("shortcuts", Shortcuts, c),
                Item::separator(),
                entry("about", About, c),
            ],
        ),
    ]
}

pub fn accelerator(action: &Action) -> Option<&'static str> {
    use Action::*;
    Some(match action {
        Open => "Ctrl+O",
        Save => "Ctrl+S",
        Copy => "Ctrl+C",
        Quit => "Ctrl+Q",
        Take => "Ctrl+N",
        Cancel => "Esc",
        Undo => "Ctrl+Z",
        Redo => "Ctrl+Shift+Z",
        Delete => "Delete",
        Properties => "Ctrl+P",
        ZoomIn => "Ctrl++",
        ZoomOut => "Ctrl+−",
        Fit => "Ctrl+0",
        Shortcuts => "F1",
        _ => return None,
    })
}

pub fn mnemonic(key: &Key, modifiers: Modifiers) -> Option<usize> {
    if modifiers != Modifiers::ALT {
        return None;
    }
    let Key::Character(key) = key else {
        return None;
    };
    ["f", "e", "c", "a", "v", "h"]
        .iter()
        .position(|letter| key.eq_ignore_ascii_case(letter))
}

pub fn shortcut(key: &Key, modifiers: Modifiers) -> Option<Action> {
    use Action::*;
    match (key, modifiers) {
        (Key::Named(Named::Delete), mods) if mods.is_empty() => Some(Delete),
        (Key::Named(Named::F1), mods) if mods.is_empty() => Some(Shortcuts),
        (Key::Character(key), mods)
            if mods == Modifiers::CTRL || mods == Modifiers::CTRL | Modifiers::SHIFT =>
        {
            let shifted = mods.shift();
            match key.to_ascii_lowercase().as_str() {
                "z" if shifted => Some(Redo),
                "z" => Some(Undo),
                "y" => Some(Redo),
                "o" => Some(Open),
                "s" => Some(Save),
                "c" => Some(Copy),
                "q" => Some(Quit),
                "n" => Some(Take),
                "p" => Some(Properties),
                "+" | "=" => Some(ZoomIn),
                "-" => Some(ZoomOut),
                "0" => Some(Fit),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn actions(items: &[Item<Action>]) -> Vec<&Item<Action>> {
        items
            .iter()
            .flat_map(|item| {
                if item.message().is_some() {
                    vec![item]
                } else {
                    actions(item.children())
                }
            })
            .collect()
    }
    #[test]
    fn every_tool_and_capture_choice_is_reachable_and_current_choices_are_checked() {
        let request = Request {
            mode: Mode::Region,
            delay: 3,
            ..Request::default()
        };
        let document = Document::new(image::RgbaImage::new(10, 10)).unwrap();
        let c = Context {
            document: Some(&document),
            selected: None,
            request: &request,
            outputs: &[],
            windows: &[],
            window: None,
            tool: Tool::Draw(Kind::Pen),
            busy: false,
            capturing: false,
            modal: false,
        };
        let bar = bar(&c);
        assert_eq!(
            bar.iter().map(|i| i.label()).collect::<Vec<_>>(),
            ["File", "Edit", "Capture", "Annotate", "View", "Help"]
        );
        let entries = actions(&bar);
        for kind in Kind::ALL {
            assert!(entries.iter().any(|i| i.message() == Some(&Action::Tool(Tool::Draw(kind))) && i.is_enabled()));
        }
        for mode in Mode::ALL {
            assert!(
                entries
                    .iter()
                    .any(|i| i.message() == Some(&Action::Mode(mode)))
            );
        }
        for action in [
            Action::Mode(Mode::Region),
            Action::Delay(3),
            Action::Tool(Tool::Draw(Kind::Pen)),
            Action::Pointer(false),
        ] {
            assert!(
                entries
                    .iter()
                    .any(|i| i.message() == Some(&action) && i.label().starts_with('✓'))
            );
        }
    }
    #[test]
    fn state_gates_editing_capture_cancellation_and_modal_commands() {
        let request = Request {
            mode: Mode::Window,
            ..Request::default()
        };
        let mut c = Context {
            document: None,
            selected: None,
            request: &request,
            outputs: &[],
            windows: &[],
            window: None,
            tool: Tool::Select,
            busy: false,
            capturing: false,
            modal: false,
        };
        for action in [
            Action::Take,
            Action::Save,
            Action::Undo,
            Action::Delete,
            Action::Pointer(true),
            Action::Cancel,
        ] {
            assert!(!enabled(&action, &c), "{action:?}");
        }
        assert!(enabled(&Action::Open, &c));
        c.busy = true;
        c.capturing = true;
        assert!(enabled(&Action::Cancel, &c));
        assert!(enabled(&Action::Quit, &c));
        assert!(!enabled(&Action::Refresh, &c));
        c.modal = true;
        assert!(!enabled(&Action::Quit, &c));
        assert!(!enabled(&Action::Cancel, &c));
    }
    #[test]
    fn shortcuts_match_displayed_commands_and_alt_mnemonics() {
        for (key, modifiers, action) in [
            ("n", Modifiers::CTRL, Action::Take),
            ("S", Modifiers::CTRL | Modifiers::SHIFT, Action::Save),
            ("z", Modifiers::CTRL | Modifiers::SHIFT, Action::Redo),
            ("p", Modifiers::CTRL, Action::Properties),
            ("+", Modifiers::CTRL | Modifiers::SHIFT, Action::ZoomIn),
            ("0", Modifiers::CTRL, Action::Fit),
        ] {
            assert_eq!(
                shortcut(&Key::Character(key.into()), modifiers),
                Some(action.clone())
            );
            assert!(accelerator(&action).is_some());
            assert_eq!(
                shortcut(&Key::Character(key.into()), modifiers | Modifiers::ALT),
                None
            );
        }
        for (index, key) in ["f", "e", "c", "a", "v", "h"].iter().enumerate() {
            assert_eq!(
                mnemonic(&Key::Character((*key).into()), Modifiers::ALT),
                Some(index)
            );
        }
    }
}
