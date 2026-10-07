// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::strings::label;
use application::iced::keyboard::{Key, Modifiers, key::Named};
use toolkit::Item;
pub const BAR_ID: &str = "busviewer-menu";
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action { Refresh, Call, Format, Clear, Copy, Quit, About, Shortcuts }
pub fn bar(busy: bool, callable: bool, modal: bool) -> Vec<Item<Action>> {
    let item = |key: &str, action: Action, enabled: bool, shortcut: &str| {
        Item::action(label(key), action).enabled(enabled && !modal).accelerator(shortcut)
    };
    vec![
        Item::submenu(label("file"),vec![item("refresh",Action::Refresh,!busy,"Ctrl+R"),Item::separator(),item("quit",Action::Quit,true,"Ctrl+Q")]),
        Item::submenu(label("edit"),vec![item("format",Action::Format,!busy,"Ctrl+Shift+F"),item("clear",Action::Clear,!busy,""),item("copy",Action::Copy,true,"")]),
        Item::submenu(label("bus"),vec![item("call",Action::Call,callable && !busy,"Ctrl+Enter")]),
        Item::submenu(label("help"),vec![item("shortcuts",Action::Shortcuts,true,"F1"),item("about",Action::About,true,"")]),
    ]
}
pub fn mnemonic(key: &Key, modifiers: Modifiers) -> Option<usize> {
    let Key::Character(c)=key else {return None;};
    (modifiers==Modifiers::ALT).then(|| ["f","e","b","h"].iter().position(|k| c.eq_ignore_ascii_case(k))).flatten()
}
pub fn shortcut(key: &Key, mods: Modifiers) -> Option<Action> {
    if mods==Modifiers::empty() && *key==Key::Named(Named::F1) { return Some(Action::Shortcuts); }
    if mods==Modifiers::CTRL && *key==Key::Named(Named::Enter) {return Some(Action::Call);}
    let Key::Character(c)=key else {return None;};
    if mods==Modifiers::CTRL {match c.to_lowercase().as_str(){"r"=>Some(Action::Refresh),"q"=>Some(Action::Quit),_=>None}}
    else if mods==(Modifiers::CTRL|Modifiers::SHIFT) && c.eq_ignore_ascii_case("f"){Some(Action::Format)}else{None}
}
