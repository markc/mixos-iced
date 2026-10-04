// SPDX-License-Identifier: MIT OR Apache-2.0
//! Keyboard routing: a key-binding model ([`Chord`], [`Bindings`],
//! [`route`]) and the root widgets that apply it ([`KeyRouter`]), capture
//! keys losslessly ([`Keys`]), keep input from the content under a modal
//! ([`Inert`]) and report where a click landed ([`FocusProbe`]).
//!
//! Keys are read by widgets, never by an event subscription. A
//! subscription's channel is bounded and drops events when the UI thread is
//! busy repainting, which is exactly when keys arrive in bursts; a widget's
//! `update` runs synchronously in event dispatch and publishes through the
//! `Shell`, so nothing is lost.
//!
//! Routing precedence, in order:
//!
//! 1. A modal dialog is open: nothing is resolved here; the dialog owns the
//!    keyboard (see [`crate::dialog`]).
//! 2. Alt+letter opens the menu with that mnemonic
//!    ([`Routed::Menu`]; see [`crate::menu::open_operation`]). F10 is not
//!    routed: the menu bar opens itself on it.
//! 3. A bound chord runs its action, unless a text field has focus and the
//!    chord is a text-editing one ([`is_text_editing`]): the field keeps
//!    those.
//! 4. Everything else goes to the children.
//! 5. A key no child captured goes to `on_unclaimed`.
//!
//! Chords are layout-aware where iced allows: a letter is taken from the
//! Latin layout position when the active layout is not Latin, so Ctrl+S
//! works on a Cyrillic layout.

use std::collections::HashMap;
use std::fmt;

use iced_core::keyboard::key::{Named, Physical};
use iced_core::keyboard::{self, Key, Modifiers};
use iced_core::widget::{Operation, Tree, tree};
use iced_core::{
    Element, Event, Layout, Length, Point, Rectangle, Shell, Size, Vector, Widget, input_method,
    layout, mouse, overlay, renderer,
};

/// Modifiers plus one key: a lower-case character (`s`, `=`, `/`) or a
/// named key (`F3`, `Tab`, `Up`, `Space`). Parsed from and displayed as
/// `Ctrl+Shift+S`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// The Super/Windows/Command key.
    pub logo: bool,
    pub key: String,
}

/// The named keys a chord may spell, with their text.
const NAMED: &[(&str, Named)] = &[
    ("F1", Named::F1),
    ("F2", Named::F2),
    ("F3", Named::F3),
    ("F4", Named::F4),
    ("F5", Named::F5),
    ("F6", Named::F6),
    ("F7", Named::F7),
    ("F8", Named::F8),
    ("F9", Named::F9),
    ("F10", Named::F10),
    ("F11", Named::F11),
    ("F12", Named::F12),
    ("Tab", Named::Tab),
    ("Space", Named::Space),
    ("PageUp", Named::PageUp),
    ("PageDown", Named::PageDown),
    ("Up", Named::ArrowUp),
    ("Down", Named::ArrowDown),
    ("Left", Named::ArrowLeft),
    ("Right", Named::ArrowRight),
    ("Home", Named::Home),
    ("End", Named::End),
    ("Backspace", Named::Backspace),
    ("Delete", Named::Delete),
    ("Insert", Named::Insert),
    ("Enter", Named::Enter),
    ("Escape", Named::Escape),
];

/// Punctuation a chord may use besides letters and digits.
const PUNCTUATION: &str = "=-/,.;'[]\\`";

fn named_text(named: Named) -> Option<&'static str> {
    NAMED
        .iter()
        .find(|(_, candidate)| *candidate == named)
        .map(|(text, _)| *text)
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || PUNCTUATION.contains(c)
}

/// The unshifted key of a shifted symbol on a US layout, so Ctrl+Shift+=
/// and Ctrl++ spell the same chord.
fn unshift(c: char) -> char {
    match c {
        '+' => '=',
        '_' => '-',
        '?' => '/',
        '<' => ',',
        '>' => '.',
        ':' => ';',
        '"' => '\'',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        '~' => '`',
        c => c,
    }
}

impl Chord {
    /// Parses `Ctrl+Alt+Shift+Super+S`: modifiers in any order (`Ctrl` or
    /// `Control`, `Alt`, `Shift`, `Super` or `Logo`, case-insensitive), the
    /// key last; a letter is case-insensitive, a named key is not.
    pub fn parse(text: &str) -> Option<Self> {
        let mut chord = Self {
            ctrl: false,
            alt: false,
            shift: false,
            logo: false,
            key: String::new(),
        };
        let parts: Vec<&str> = text.split('+').collect();
        let (key, modifiers) = parts.split_last()?;
        for modifier in modifiers {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => chord.ctrl = true,
                "alt" => chord.alt = true,
                "shift" => chord.shift = true,
                "super" | "logo" => chord.logo = true,
                _ => return None,
            }
        }
        let key = *key;
        let mut chars = key.chars();
        let single = chars.next().filter(|_| chars.next().is_none());
        chord.key = match single {
            Some(c) if is_key_char(c) => c.to_ascii_lowercase().to_string(),
            Some(_) => return None,
            None => {
                NAMED.iter().find(|(text, _)| *text == key)?;
                key.to_owned()
            }
        };
        Some(chord)
    }

    /// The chord a key press spells, or `None` for a modifier, a dead key
    /// or a character outside the chord alphabet. A letter comes from the
    /// Latin layout position when the active layout is not Latin.
    pub fn from_key(key: &Key, physical: Physical, modifiers: Modifiers) -> Option<Self> {
        let name = match key {
            Key::Named(named) => named_text(*named)?.to_owned(),
            Key::Character(s) => {
                let c = key.to_latin(physical).or_else(|| s.chars().next())?;
                let c = unshift(c);
                if !is_key_char(c) {
                    return None;
                }
                c.to_ascii_lowercase().to_string()
            }
            Key::Unidentified => return None,
        };
        Some(Self {
            ctrl: modifiers.control(),
            alt: modifiers.alt(),
            shift: modifiers.shift(),
            logo: modifiers.logo(),
            key: name,
        })
    }

    /// The chord of a `KeyPressed` event, if the event is one.
    pub fn from_event(event: &keyboard::Event) -> Option<Self> {
        match event {
            keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                ..
            } => Self::from_key(key, *physical_key, *modifiers),
            _ => None,
        }
    }

    /// The `Named` key this chord spells, if it is a named key.
    pub fn named(&self) -> Option<Named> {
        NAMED
            .iter()
            .find(|(text, _)| *text == self.key)
            .map(|(_, named)| *named)
    }

    /// Whether any modifier is held.
    pub fn is_modified(&self) -> bool {
        self.ctrl || self.alt || self.shift || self.logo
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        if self.logo {
            f.write_str("Super+")?;
        }
        let mut chars = self.key.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => write!(f, "{}", c.to_ascii_uppercase()),
            _ => f.write_str(&self.key),
        }
    }
}

impl std::str::FromStr for Chord {
    type Err = BindError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text).ok_or_else(|| BindError::Unparsable(text.to_owned()))
    }
}

/// Why a binding was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindError {
    /// The chord text does not parse.
    Unparsable(String),
    /// The chord is already bound.
    Taken(String),
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unparsable(text) => write!(f, "chord {text:?} does not parse"),
            Self::Taken(text) => write!(f, "chord {text} is already bound"),
        }
    }
}

impl std::error::Error for BindError {}

/// Chord → action, in table order. A chord maps to exactly one action; an
/// action may have several chords, and its first one is its accelerator
/// label ([`Bindings::label`]).
#[derive(Debug, Clone)]
pub struct Bindings<A> {
    table: Vec<(Chord, A)>,
    index: HashMap<Chord, usize>,
}

impl<A> Default for Bindings<A> {
    fn default() -> Self {
        Self {
            table: Vec::new(),
            index: HashMap::new(),
        }
    }
}

impl<A> Bindings<A> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds every `(chord text, action)` row, refusing the table at the
    /// first row that does not parse or repeats a chord.
    pub fn from_table<'a>(rows: impl IntoIterator<Item = (&'a str, A)>) -> Result<Self, BindError> {
        let mut bindings = Self::new();
        for (text, action) in rows {
            bindings.bind(text, action)?;
        }
        Ok(bindings)
    }

    /// Binds `chord` to `action` unless the chord is malformed or taken.
    pub fn bind(&mut self, chord: &str, action: A) -> Result<(), BindError> {
        let parsed = Chord::parse(chord).ok_or_else(|| BindError::Unparsable(chord.to_owned()))?;
        if self.index.contains_key(&parsed) {
            return Err(BindError::Taken(parsed.to_string()));
        }
        self.set(parsed, action);
        Ok(())
    }

    /// Binds `chord` to `action`, replacing any binding it had.
    pub fn set(&mut self, chord: Chord, action: A) {
        match self.index.get(&chord) {
            Some(&at) => self.table[at].1 = action,
            None => {
                self.index.insert(chord.clone(), self.table.len());
                self.table.push((chord, action));
            }
        }
    }

    /// Removes the binding of `chord`, returning its action.
    pub fn unbind(&mut self, chord: &Chord) -> Option<A> {
        let at = self.index.remove(chord)?;
        let (_, action) = self.table.remove(at);
        for (chord, _) in &self.table[at..] {
            if let Some(index) = self.index.get_mut(chord) {
                *index -= 1;
            }
        }
        Some(action)
    }

    pub fn get(&self, chord: &Chord) -> Option<&A> {
        self.index.get(chord).map(|&at| &self.table[at].1)
    }

    /// Every chord bound to `action`, in table order.
    pub fn chords_for<'a>(&'a self, action: &'a A) -> impl Iterator<Item = &'a Chord> + 'a
    where
        A: PartialEq,
    {
        self.table
            .iter()
            .filter(move |(_, bound)| bound == action)
            .map(|(chord, _)| chord)
    }

    /// The accelerator label of `action`: its first chord, displayed.
    pub fn label(&self, action: &A) -> Option<String>
    where
        A: PartialEq,
    {
        self.chords_for(action).next().map(ToString::to_string)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Chord, &A)> {
        self.table.iter().map(|(chord, action)| (chord, action))
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }
}

/// Chords a focused text field needs for itself.
pub fn is_text_editing(chord: &Chord) -> bool {
    let key = chord.key.as_str();
    if chord.ctrl && !chord.alt {
        return matches!(
            key,
            "a" | "c"
                | "v"
                | "x"
                | "z"
                | "y"
                | "Backspace"
                | "Delete"
                | "Insert"
                | "Left"
                | "Right"
                | "Home"
                | "End"
        );
    }
    !chord.ctrl && !chord.alt && matches!(key, "Insert" | "Delete")
}

/// Alt+letter without Ctrl → the index of that letter in `mnemonics` (the
/// menu bar's entries, in order).
pub fn mnemonic(chord: &Chord, mnemonics: &[char]) -> Option<usize> {
    if !chord.alt || chord.ctrl || chord.shift || chord.logo {
        return None;
    }
    let mut chars = chord.key.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    mnemonics
        .iter()
        .position(|m| m.to_ascii_lowercase() == c)
}

/// What the router knows about the window when a key arrives.
#[derive(Debug, Clone, Copy, Default)]
pub struct Context<'a> {
    /// A modal dialog is open: nothing is routed.
    pub modal: bool,
    /// A text field has keyboard focus: it keeps the editing chords.
    pub text_field: bool,
    /// The menu bar's mnemonics, in bar order, for Alt+letter.
    pub mnemonics: &'a [char],
}

/// What the router decided for a key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Routed<A> {
    /// Alt+letter: open menu bar entry `index`.
    Menu(usize),
    /// A bound chord.
    Action(A),
}

/// The pure routing decision (tested without a widget tree).
pub fn route<A: Clone>(bindings: &Bindings<A>, chord: &Chord, context: Context<'_>) -> Option<Routed<A>> {
    if context.modal {
        return None;
    }
    if let Some(index) = mnemonic(chord, context.mnemonics) {
        return Some(Routed::Menu(index));
    }
    if context.text_field && is_text_editing(chord) {
        return None;
    }
    bindings.get(chord).cloned().map(Routed::Action)
}

/// The `Widget` methods a wrapper passes straight to its content, which it
/// shares its tree node with (same tag and state).
macro_rules! forward_to_content {
    () => {
        fn tag(&self) -> tree::Tag {
            self.content.as_widget().tag()
        }

        fn state(&self) -> tree::State {
            self.content.as_widget().state()
        }

        fn diff(&mut self, tree: &mut Tree) {
            self.content.as_widget_mut().diff(tree);
        }

        fn size(&self) -> Size<Length> {
            self.content.as_widget().size()
        }

        fn layout(
            &mut self,
            tree: &mut Tree,
            renderer: &Renderer,
            limits: &layout::Limits,
        ) -> layout::Node {
            self.content.as_widget_mut().layout(tree, renderer, limits)
        }

        fn operate(
            &mut self,
            tree: &mut Tree,
            layout: Layout<'_>,
            renderer: &Renderer,
            operation: &mut dyn Operation,
        ) {
            self.content
                .as_widget_mut()
                .operate(tree, layout, renderer, operation);
        }

        fn draw(
            &self,
            tree: &Tree,
            renderer: &mut Renderer,
            theme: &Theme,
            style: &renderer::Style,
            layout: Layout<'_>,
            cursor: mouse::Cursor,
            viewport: &Rectangle,
        ) {
            self.content
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
        }

        fn mouse_interaction(
            &self,
            tree: &Tree,
            layout: Layout<'_>,
            cursor: mouse::Cursor,
            viewport: &Rectangle,
            renderer: &Renderer,
        ) -> mouse::Interaction {
            self.content
                .as_widget()
                .mouse_interaction(tree, layout, cursor, viewport, renderer)
        }

        fn overlay<'b>(
            &'b mut self,
            tree: &'b mut Tree,
            layout: Layout<'b>,
            renderer: &Renderer,
            viewport: &Rectangle,
            translation: Vector,
        ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
            self.content
                .as_widget_mut()
                .overlay(tree, layout, renderer, viewport, translation)
        }
    };
}

pub(crate) use forward_to_content;

type RouteFn<'a, A, Message> = Box<dyn Fn(Routed<A>) -> Message + 'a>;
type UnclaimedFn<'a, Message> = Box<dyn Fn(&Key, Modifiers) -> Option<Message> + 'a>;
type ZoomFn<'a, Message> = Box<dyn Fn(f32) -> Message + 'a>;

/// Wraps the whole window content and resolves chords before its children
/// see them.
pub struct KeyRouter<'a, A, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    bindings: &'a Bindings<A>,
    modal: bool,
    text_field: bool,
    mnemonics: Vec<char>,
    on_route: RouteFn<'a, A, Message>,
    on_unclaimed: Option<UnclaimedFn<'a, Message>>,
    on_zoom: Option<ZoomFn<'a, Message>>,
}

/// Wraps `content`; a routed chord publishes `on_route`'s message.
pub fn router<'a, A, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    bindings: &'a Bindings<A>,
    on_route: impl Fn(Routed<A>) -> Message + 'a,
) -> KeyRouter<'a, A, Message, Theme, Renderer> {
    KeyRouter {
        content: content.into(),
        bindings,
        modal: false,
        text_field: false,
        mnemonics: Vec::new(),
        on_route: Box::new(on_route),
        on_unclaimed: None,
        on_zoom: None,
    }
}

impl<'a, A, Message, Theme, Renderer> KeyRouter<'a, A, Message, Theme, Renderer> {
    /// A modal dialog is open: no chord is routed.
    pub fn modal(mut self, modal: bool) -> Self {
        self.modal = modal;
        self
    }

    /// A text field has keyboard focus: it keeps the editing chords.
    pub fn text_field(mut self, focused: bool) -> Self {
        self.text_field = focused;
        self
    }

    /// The menu bar's mnemonics in bar order; Alt+letter routes
    /// [`Routed::Menu`].
    pub fn mnemonics(mut self, mnemonics: impl IntoIterator<Item = char>) -> Self {
        self.mnemonics = mnemonics.into_iter().collect();
        self
    }

    /// Called for a key press no child captured.
    pub fn on_unclaimed(mut self, f: impl Fn(&Key, Modifiers) -> Option<Message> + 'a) -> Self {
        self.on_unclaimed = Some(Box::new(f));
        self
    }

    /// Ctrl+wheel: published with the vertical line delta (positive is in).
    pub fn on_zoom(mut self, f: impl Fn(f32) -> Message + 'a) -> Self {
        self.on_zoom = Some(Box::new(f));
        self
    }

    fn context(&self) -> Context<'_> {
        Context {
            modal: self.modal,
            text_field: self.text_field,
            mnemonics: &self.mnemonics,
        }
    }
}

impl<A, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for KeyRouter<'_, A, Message, Theme, Renderer>
where
    A: Clone,
    Renderer: iced_core::Renderer,
{
    forward_to_content!();

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        match event {
            Event::Keyboard(key_event @ keyboard::Event::KeyPressed { .. }) => {
                if let Some(chord) = Chord::from_event(key_event)
                    && let Some(routed) = route(self.bindings, &chord, self.context())
                {
                    shell.publish((self.on_route)(routed));
                    shell.capture_event();
                    return;
                }
            }
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                MODIFIERS.with(|cell| cell.set(*modifiers));
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if !self.modal => {
                if let Some(zoom) = &self.on_zoom
                    && cursor.is_over(layout.bounds())
                    && MODIFIERS.with(|cell| cell.get().control())
                {
                    let lines = match delta {
                        mouse::ScrollDelta::Lines { y, .. } => *y,
                        mouse::ScrollDelta::Pixels { y, .. } => *y / 40.0,
                    };
                    shell.publish(zoom(lines));
                    shell.capture_event();
                    return;
                }
            }
            _ => {}
        }
        self.content
            .as_widget_mut()
            .update(tree, event, layout, cursor, renderer, shell, viewport);
        if !shell.is_event_captured()
            && let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event
            && let Some(f) = &self.on_unclaimed
            && let Some(message) = f(key, *modifiers)
        {
            shell.publish(message);
            shell.capture_event();
        }
    }
}

impl<'a, A, Message, Theme, Renderer> From<KeyRouter<'a, A, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    A: Clone + 'a,
    Message: 'a,
    Theme: 'a,
    Renderer: iced_core::Renderer + 'a,
{
    fn from(router: KeyRouter<'a, A, Message, Theme, Renderer>) -> Self {
        Element::new(router)
    }
}

thread_local! {
    /// Modifier state as last reported, for wheel events, which carry none.
    /// The UI runs on one thread, so a thread-local is exact.
    static MODIFIERS: std::cell::Cell<Modifiers> = const { std::cell::Cell::new(Modifiers::empty()) };
}

type KeyHandler<'a, Message> = Box<dyn Fn(&keyboard::Event) -> Option<Message> + 'a>;
type ImeHandler<'a, Message> = Box<dyn Fn(&input_method::Event) -> Message + 'a>;
type PointerHandler<'a, Message> = Box<dyn Fn(Point) -> Option<Message> + 'a>;
type MouseHandler<'a, Message> = Box<dyn Fn(&mouse::Event, Option<Point>) -> Option<Message> + 'a>;
type Redraw<Message> = (
    Option<iced_core::time::Instant>,
    fn(iced_core::time::Instant) -> Message,
);

/// Wraps `content` and reports every keyboard event a child did not
/// capture, losslessly: the root of a terminal or any widget that reads
/// raw keys. IME, mouse and redraw reporting share the same synchronous
/// path.
pub struct Keys<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    on_press: KeyHandler<'a, Message>,
    on_ime: Option<ImeHandler<'a, Message>>,
    ime: input_method::InputMethod,
    on_pointer: Option<PointerHandler<'a, Message>>,
    on_mouse: Option<MouseHandler<'a, Message>>,
    redraw: Option<Redraw<Message>>,
}

/// Wraps `content` so `on_press` sees every keyboard event its children
/// leave.
pub fn keys<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    on_press: impl Fn(&keyboard::Event) -> Option<Message> + 'a,
) -> Keys<'a, Message, Theme, Renderer> {
    Keys {
        content: content.into(),
        on_press: Box::new(on_press),
        on_ime: None,
        ime: input_method::InputMethod::Disabled,
        on_pointer: None,
        on_mouse: None,
        redraw: None,
    }
}

impl<'a, Message, Theme, Renderer> Keys<'a, Message, Theme, Renderer> {
    /// Requests `ime` on every redraw (after the children, so a focused
    /// text input's own request wins) and reports IME events while it is
    /// enabled, plus the closing event.
    pub fn input_method(
        mut self,
        ime: input_method::InputMethod,
        callback: impl Fn(&input_method::Event) -> Message + 'a,
    ) -> Self {
        self.ime = ime;
        self.on_ime = Some(Box::new(callback));
        self
    }

    /// Mouse events before the children see them; a `Some` claims the
    /// event, so the callback should claim only what it owns.
    pub fn on_mouse(
        mut self,
        callback: impl Fn(&mouse::Event, Option<Point>) -> Option<Message> + 'a,
    ) -> Self {
        self.on_mouse = Some(Box::new(callback));
        self
    }

    /// Pointer motion, without claiming it.
    pub fn on_pointer(mut self, callback: impl Fn(Point) -> Option<Message> + 'a) -> Self {
        self.on_pointer = Some(Box::new(callback));
        self
    }

    /// Reports each redraw instant once: `last` is the instant already
    /// handled, so the runtime's redraw retry is not reported twice.
    pub fn on_redraw(
        mut self,
        last: Option<iced_core::time::Instant>,
        message: fn(iced_core::time::Instant) -> Message,
    ) -> Self {
        self.redraw = Some((last, message));
        self
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Keys<'_, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    forward_to_content!();

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if let Event::Mouse(mouse::Event::CursorMoved { position }) = event
            && let Some(callback) = &self.on_pointer
            && let Some(message) = callback(*position)
        {
            shell.publish(message);
        }
        if let Event::Window(iced_core::window::Event::RedrawRequested(at)) = event
            && let Some((last, message)) = self.redraw
            && last != Some(*at)
        {
            shell.publish(message(*at));
        }
        if let Event::Mouse(mouse_event) = event
            && let Some(callback) = &self.on_mouse
            && let Some(message) = callback(mouse_event, cursor.position())
        {
            shell.publish(message);
            shell.capture_event();
            return;
        }
        self.content
            .as_widget_mut()
            .update(tree, event, layout, cursor, renderer, shell, viewport);
        // After the children: a focused text input's IME request wins.
        if matches!(event, Event::Window(iced_core::window::Event::RedrawRequested(_))) {
            shell.request_input_method(&self.ime);
        }
        if shell.is_event_captured() {
            return;
        }
        if let Event::InputMethod(ime_event) = event
            && (self.ime.is_enabled() || matches!(ime_event, input_method::Event::Closed))
            && let Some(callback) = &self.on_ime
        {
            shell.publish(callback(ime_event));
            shell.capture_event();
            return;
        }
        if let Event::Keyboard(key_event) = event
            && let Some(message) = (self.on_press)(key_event)
        {
            shell.publish(message);
            shell.capture_event();
        }
    }
}

impl<'a, Message, Theme, Renderer> From<Keys<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced_core::Renderer + 'a,
{
    fn from(keys: Keys<'a, Message, Theme, Renderer>) -> Self {
        Element::new(keys)
    }
}

/// Shows `content` but keeps keyboard and IME input from it: what sits
/// under a modal dialog, so typing into the dialog never also types into
/// the content behind it. Mouse and window events still pass.
pub struct Inert<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
}

pub fn inert<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
) -> Inert<'a, Message, Theme, Renderer> {
    Inert {
        content: content.into(),
    }
}

/// Whether `event` is keyboard or IME input, which [`Inert`] withholds.
pub fn is_input(event: &Event) -> bool {
    matches!(
        event,
        Event::Keyboard(keyboard::Event::KeyPressed { .. } | keyboard::Event::KeyReleased { .. })
            | Event::InputMethod(_)
    )
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Inert<'_, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    forward_to_content!();

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if !is_input(event) {
            self.content
                .as_widget_mut()
                .update(tree, event, layout, cursor, renderer, shell, viewport);
        }
    }
}

impl<'a, Message, Theme, Renderer> From<Inert<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced_core::Renderer + 'a,
{
    fn from(inert: Inert<'a, Message, Theme, Renderer>) -> Self {
        Element::new(inert)
    }
}

/// Publishes `message` when a mouse button goes down over `content`, before
/// the content sees it and without capturing it: how an application learns
/// that a text field took keyboard focus (iced text inputs do not say).
pub struct FocusProbe<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    message: Message,
}

pub fn focus_probe<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    message: Message,
) -> FocusProbe<'a, Message, Theme, Renderer> {
    FocusProbe {
        content: content.into(),
        message,
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for FocusProbe<'_, Message, Theme, Renderer>
where
    Message: Clone,
    Renderer: iced_core::Renderer,
{
    forward_to_content!();

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if let Event::Mouse(mouse::Event::ButtonPressed(_)) = event
            && cursor.is_over(layout.bounds())
        {
            shell.publish(self.message.clone());
        }
        self.content
            .as_widget_mut()
            .update(tree, event, layout, cursor, renderer, shell, viewport);
    }
}

impl<'a, Message, Theme, Renderer> From<FocusProbe<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
    Theme: 'a,
    Renderer: iced_core::Renderer + 'a,
{
    fn from(probe: FocusProbe<'a, Message, Theme, Renderer>) -> Self {
        Element::new(probe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_core::keyboard::key::{Code, NativeCode};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Action {
        Save,
        Find,
        Next,
        ZoomIn,
    }

    const TABLE: [(&str, Action); 6] = [
        ("Ctrl+S", Action::Save),
        ("Ctrl+F", Action::Find),
        ("F3", Action::Next),
        ("Ctrl+Shift+F", Action::Next),
        ("Ctrl+=", Action::ZoomIn),
        ("Ctrl+Shift+=", Action::ZoomIn),
    ];

    fn chord(text: &str) -> Chord {
        Chord::parse(text).unwrap_or_else(|| panic!("{text} parses"))
    }

    fn bindings() -> Bindings<Action> {
        Bindings::from_table(TABLE).unwrap()
    }

    fn unidentified() -> Physical {
        Physical::Unidentified(NativeCode::Unidentified)
    }

    #[test]
    fn chords_parse_and_display_round_trip() {
        for text in [
            "Ctrl+S",
            "Ctrl+Alt+Shift+Super+F12",
            "Alt+Up",
            "Shift+Tab",
            "Space",
            "Ctrl+/",
            "Ctrl+=",
            "F3",
        ] {
            assert_eq!(chord(text).to_string(), text);
        }
        assert_eq!(chord("ctrl+s"), chord("Ctrl+S"));
        assert_eq!(chord("Control+Logo+Up"), chord("Ctrl+Super+Up"));
        assert_eq!(chord("Ctrl+s"), chord("Ctrl+S"));
        assert_eq!(chord("Up").named(), Some(Named::ArrowUp));
        assert_eq!(chord("a").named(), None);
        assert!(!chord("F3").is_modified());
        assert!(chord("Shift+F3").is_modified());
        for bad in ["Meta+S", "Ctrl+", "Ctrl+Foo", "é", "Ctrl+Shift", ""] {
            assert_eq!(Chord::parse(bad), None, "{bad:?}");
        }
        assert_eq!(
            "Ctrl+Nope".parse::<Chord>(),
            Err(BindError::Unparsable("Ctrl+Nope".into()))
        );
    }

    #[test]
    fn key_events_spell_chords_from_the_latin_position() {
        let character = |s: &str| Key::Character(s.into());
        let ctrl = Modifiers::CTRL;
        assert_eq!(
            Chord::from_key(&character("s"), unidentified(), ctrl),
            Some(chord("Ctrl+S"))
        );
        assert_eq!(
            Chord::from_key(&character("S"), unidentified(), ctrl | Modifiers::SHIFT),
            Some(chord("Ctrl+Shift+S"))
        );
        assert_eq!(
            Chord::from_key(&character("+"), unidentified(), ctrl | Modifiers::SHIFT),
            Some(chord("Ctrl+Shift+="))
        );
        assert_eq!(
            Chord::from_key(&Key::Named(Named::F3), unidentified(), Modifiers::SHIFT),
            Some(chord("Shift+F3"))
        );
        assert_eq!(
            Chord::from_key(&Key::Named(Named::ArrowUp), unidentified(), ctrl | Modifiers::LOGO),
            Some(chord("Ctrl+Super+Up"))
        );
        // A Cyrillic layout: the logical key is с, the physical key is C.
        assert_eq!(
            Chord::from_key(&character("с"), Physical::Code(Code::KeyC), ctrl),
            Some(chord("Ctrl+C"))
        );
        assert_eq!(
            Chord::from_key(&character("с"), unidentified(), ctrl),
            None,
            "no physical key to translate from"
        );
        assert_eq!(
            Chord::from_key(&Key::Named(Named::Shift), unidentified(), Modifiers::SHIFT),
            None
        );
        assert_eq!(
            Chord::from_event(&keyboard::Event::ModifiersChanged(ctrl)),
            None
        );
    }

    #[test]
    fn bindings_keep_table_order_and_refuse_duplicates() {
        let b = bindings();
        assert_eq!(b.len(), 6);
        assert_eq!(b.get(&chord("Ctrl+S")), Some(&Action::Save));
        assert_eq!(
            b.chords_for(&Action::Next)
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["F3", "Ctrl+Shift+F"]
        );
        assert_eq!(b.label(&Action::ZoomIn).as_deref(), Some("Ctrl+="));
        assert_eq!(b.label(&Action::Save).as_deref(), Some("Ctrl+S"));

        let mut b = b;
        assert_eq!(
            b.bind("Ctrl+S", Action::Find),
            Err(BindError::Taken("Ctrl+S".into()))
        );
        assert_eq!(
            b.bind("Ctrl+Nope", Action::Find),
            Err(BindError::Unparsable("Ctrl+Nope".into()))
        );
        b.set(chord("Ctrl+S"), Action::Find);
        assert_eq!(b.get(&chord("Ctrl+S")), Some(&Action::Find));
        assert_eq!(b.len(), 6, "set replaces in place");
        assert_eq!(b.unbind(&chord("Ctrl+F")), Some(Action::Find));
        assert_eq!(b.unbind(&chord("Ctrl+F")), None);
        assert_eq!(b.get(&chord("Ctrl+Shift+=")), Some(&Action::ZoomIn), "later rows reindexed");
        assert_eq!(b.get(&chord("F3")), Some(&Action::Next));
        assert_eq!(b.iter().count(), 5);
        assert_eq!(
            Bindings::from_table([("Ctrl+S", 1), ("Ctrl+S", 2)]).err(),
            Some(BindError::Taken("Ctrl+S".into()))
        );
        assert!(Bindings::<u8>::new().is_empty());
    }

    #[test]
    fn routing_follows_the_precedence() {
        let b = bindings();
        let plain = Context::default();
        let menus = ['f', 'e', 'h'];
        let with_menus = Context {
            mnemonics: &menus,
            ..plain
        };
        assert_eq!(
            route(&b, &chord("Ctrl+S"), plain),
            Some(Routed::Action(Action::Save))
        );
        assert_eq!(route(&b, &chord("Ctrl+Q"), plain), None, "unbound");
        assert_eq!(
            route(&b, &chord("Alt+E"), with_menus),
            Some(Routed::Menu(1)),
            "mnemonics open menus"
        );
        assert_eq!(route(&b, &chord("Alt+E"), plain), None, "no bar, no mnemonic");
        assert_eq!(
            route(&b, &chord("Ctrl+Alt+F"), with_menus),
            None,
            "Ctrl+Alt is not a mnemonic"
        );
        let modal = Context {
            modal: true,
            ..with_menus
        };
        assert_eq!(route(&b, &chord("Ctrl+S"), modal), None, "a dialog owns the keyboard");
        assert_eq!(route(&b, &chord("Alt+F"), modal), None);
        let field = Context {
            text_field: true,
            ..plain
        };
        let mut editing = bindings();
        editing.bind("Ctrl+V", Action::Find).unwrap();
        editing.bind("Ctrl+Z", Action::Find).unwrap();
        assert_eq!(route(&editing, &chord("Ctrl+V"), field), None, "the field pastes");
        assert_eq!(route(&editing, &chord("Ctrl+Z"), field), None);
        assert_eq!(
            route(&editing, &chord("F3"), field),
            Some(Routed::Action(Action::Next)),
            "F3 still routes from inside the field"
        );
        assert_eq!(
            route(&editing, &chord("Ctrl+S"), field),
            Some(Routed::Action(Action::Save))
        );
        assert!(is_text_editing(&chord("Shift+Insert")));
        assert!(is_text_editing(&chord("Ctrl+Backspace")));
        assert!(!is_text_editing(&chord("Ctrl+Alt+V")));
        assert_eq!(mnemonic(&chord("Alt+H"), &['F', 'E', 'H']), Some(2));
        assert_eq!(mnemonic(&chord("Alt+F3"), &['f']), None);
    }

    #[test]
    fn inert_withholds_only_input() {
        let press = Event::Keyboard(keyboard::Event::KeyPressed {
            key: Key::Named(Named::Enter),
            modified_key: Key::Named(Named::Enter),
            physical_key: unidentified(),
            location: keyboard::Location::Standard,
            modifiers: Modifiers::empty(),
            text: None,
            repeat: false,
        });
        assert!(is_input(&press));
        assert!(is_input(&Event::InputMethod(input_method::Event::Closed)));
        assert!(!is_input(&Event::Mouse(mouse::Event::CursorEntered)));
        assert!(!is_input(&Event::Keyboard(keyboard::Event::ModifiersChanged(
            Modifiers::empty()
        ))));
    }
}
