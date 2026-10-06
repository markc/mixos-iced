// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{model::{EDGES, Selection, Snapshot, View, rows}, strings::label};
use application::iced::keyboard::{Key, Modifiers, key::Named};
use serde_json::{Value, json};
use toolkit::menu::Item;

pub const BAR_ID:&str = "scene-editor-menu-bar";
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Remote(&'static str), Move(&'static str), Mode(&'static str,&'static str),
    Size(&'static str,&'static str), Order(&'static str,&'static str),
    View(View), Edge(&'static str), Refresh, Quit, Shortcuts, About,
}
impl Action {
    pub fn request(&self) -> Option<Value> {
        Some(match self {
            Self::Remote(action) => json!({"action":action}),
            Self::Move(edge) => json!({"action":"move","edge":edge}),
            Self::Mode(edge,mode) => json!({"action":"mode","edge":edge,"mode":mode}),
            Self::Size(edge,direction) => json!({"action":"size","edge":edge,"direction":direction}),
            Self::Order(edge,direction) => json!({"action":"order","edge":edge,"direction":direction}),
            _ => return None,
        })
    }
    pub fn confirmation(&self) -> Option<&'static str> {
        match self { Self::Remote("remove")=>Some("confirm-remove"),Self::Remote("reset")=>Some("confirm-reset"),Self::Remote("promote")=>Some("confirm-promote"),_=>None }
    }
}
pub struct Context<'a> {
    pub selection:&'a Selection,
    pub snapshot:&'a Snapshot,
    pub edge:&'a str,
    pub busy:bool,
    pub modal:bool,
}
pub fn enabled(action:&Action,c:&Context<'_>) -> bool {
    if c.modal { return false; }
    if matches!(action,Action::View(_) | Action::Edge(_) | Action::Quit) { return true; }
    if c.busy { return false; }
    if matches!(action,Action::Refresh | Action::Shortcuts | Action::About) { return true; }
    let scene = c.snapshot.scene(c.selection);
    if matches!(action,Action::Remote("edit-scene")) { return scene.is_some_and(|s|s["files"]["scene"].is_string()); }
    if matches!(action,Action::Remote("edit-behaviour")) { return scene.is_some_and(|s|s["files"]["behaviour"].is_string()); }
    if !c.snapshot.writable() { return false; }
    match action {
        Action::Remote("install" | "install-enable") => c.snapshot.template(c.selection).is_some_and(|t|t["diagnostic"].is_null()),
        Action::Remote("recommended") => c.snapshot.model()["recommended"]["shown"].as_bool() == Some(true),
        Action::Remote("promote") => scene.is_some_and(|s|s["forked_from"].is_string()),
        Action::Remote("fork") => scene.is_some_and(|s|s["installed"].as_bool()==Some(true) && s["forked_from"].is_null()),
        Action::Remote("reset") => scene.is_some_and(|s|s["template"].is_string()),
        Action::Remote(_) => scene.is_some(),
        Action::Move(edge) => scene.is_some_and(|s|s["kind"]!="dialog" && s["edge"].as_str()!=Some(edge)),
        Action::Mode(edge,_) | Action::Size(edge,_) => c.snapshot.0["panels"][edge].is_object() && c.snapshot.0["host_problem"].is_null(),
        Action::Order(edge,_) => c.selection.page.as_ref().is_some_and(|p|p.edge==*edge && rows(&c.snapshot.model()["edges"][edge]["pages"]).iter().any(|r|r["page"].as_str()==Some(p.page.as_str()))) && c.snapshot.0["host_problem"].is_null(),
        _=>true,
    }
}
fn item(key:&str,a:Action,c:&Context<'_>) -> Item<Action> {
    let mut item = Item::action(label(key),a.clone()).enabled(enabled(&a,c));
    if let Some(chord) = accelerator(&a) { item = item.accelerator(chord); }
    item
}
fn checked(key:&str,a:Action,on:bool,c:&Context<'_>) -> Item<Action> {
    Item::action(format!("{} {}",if on {"✓"}else{"\u{2007}"},label(key)),a.clone()).enabled(enabled(&a,c))
}
pub fn bar(c:&Context<'_>) -> Vec<Item<Action>> {
    use Action::*;
    let selected = c.snapshot.scene(c.selection);
    let edge = EDGES.into_iter().find(|e|*e==c.edge).unwrap_or("bottom");
    vec![
        Item::submenu(label("menu-file"),vec![item("install",Remote("install"),c),item("install-enable",Remote("install-enable"),c),item("recommended",Remote("recommended"),c),Item::separator(),item("refresh",Refresh,c),Item::separator(),item("quit",Quit,c)]),
        Item::submenu(label("menu-edit"),vec![item("edit-scene",Remote("edit-scene"),c),item("edit-behaviour",Remote("edit-behaviour"),c)]),
        Item::submenu(label("menu-scene"),vec![
            item(if selected.is_some_and(|s|s["enabled"]==true){"disable"}else{"enable"},Remote("toggle"),c),
            item("reload",Remote("reload"),c),Item::separator(),item("fork",Remote("fork"),c),item("promote",Remote("promote"),c),
            Item::submenu(label("move"),EDGES.into_iter().map(|edge|checked(edge,Move(edge),selected.is_some_and(|s|s["edge"]==edge),c)).collect()),
            Item::separator(),item("reset",Remote("reset"),c),item("remove",Remote("remove"),c)]),
        Item::submenu(label("menu-arrange"),vec![
            Item::submenu(label("edge"),EDGES.into_iter().map(|edge|checked(edge,Edge(edge),c.edge==edge,c)).collect()),
            Item::submenu(label("mode"),["hidden","pinned","docked"].into_iter().map(|mode|checked(mode,Mode(edge,mode),c.snapshot.0["panels"][edge]["mode"]==mode,c)).collect()),
            Item::separator(),item("smaller",Size(edge,"minus"),c),item("larger",Size(edge,"plus"),c),
            Item::separator(),item("earlier",Order(edge,"up"),c),item("later",Order(edge,"down"),c)]),
        Item::submenu(label("menu-view"),crate::model::View::ALL.into_iter().map(|view|checked(view.key(),View(view),c.selection.view==view,c)).collect()),
        Item::submenu(label("menu-help"),vec![item("shortcuts",Shortcuts,c),item("about",About,c)]),
    ].into_iter().map(|item|item.enabled(!c.modal)).collect()
}
pub fn accelerator(action:&Action) -> Option<&'static str> {
    match action { Action::Refresh=>Some("Ctrl+R"),Action::Quit=>Some("Ctrl+Q"),Action::Remote("edit-scene")=>Some("Ctrl+O"),Action::Remote("edit-behaviour")=>Some("Ctrl+Shift+O"),Action::View(View::Gallery)=>Some("Ctrl+1"),Action::View(View::Installed)=>Some("Ctrl+2"),Action::View(View::Arrange)=>Some("Ctrl+3"),Action::Shortcuts=>Some("F1"),_=>None }
}
pub fn shortcut(key:&Key,mods:Modifiers) -> Option<Action> {
    if mods==Modifiers::empty() && *key==Key::Named(Named::F1) { return Some(Action::Shortcuts); }
    let Key::Character(value)=key else{return None};
    if mods==Modifiers::CTRL { return match value.to_lowercase().as_str(){"r"=>Some(Action::Refresh),"q"=>Some(Action::Quit),"o"=>Some(Action::Remote("edit-scene")),"1"=>Some(Action::View(View::Gallery)),"2"=>Some(Action::View(View::Installed)),"3"=>Some(Action::View(View::Arrange)),_=>None}; }
    if mods==(Modifiers::CTRL | Modifiers::SHIFT) && value.eq_ignore_ascii_case("o") { return Some(Action::Remote("edit-behaviour")); }
    None
}
pub fn mnemonic(key:&Key,mods:Modifiers)->Option<usize> {
    let Key::Character(value)=key else{return None};
    (mods==Modifiers::ALT).then(||["f","e","s","a","v","h"].iter().position(|k|value.eq_ignore_ascii_case(k))).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_and_modal_state_prevent_mutations() {
        let selection=Selection::default();let snapshot=Snapshot::default();
        let mut c=Context{selection:&selection,snapshot:&snapshot,edge:"bottom",busy:false,modal:false};
        for action in [Action::Remote("remove"),Action::Remote("install"),Action::Mode("bottom","pinned"),Action::Order("bottom","down")] { assert!(!enabled(&action,&c)); }
        assert!(enabled(&Action::Refresh,&c));c.modal=true;
        for action in [Action::Refresh,Action::Quit,Action::View(View::Installed)] { assert!(!enabled(&action,&c)); }
    }
    #[test]
    fn displayed_shortcuts_and_alt_menus_are_consistent() {
        assert_eq!(shortcut(&Key::Character("O".into()),Modifiers::CTRL|Modifiers::SHIFT),Some(Action::Remote("edit-behaviour")));
        for (i,key) in ["f","e","s","a","v","h"].into_iter().enumerate() { assert_eq!(mnemonic(&Key::Character(key.into()),Modifiers::ALT),Some(i)); }
        assert_eq!(shortcut(&Key::Character("q".into()),Modifiers::CTRL|Modifiers::ALT),None);
    }
}
