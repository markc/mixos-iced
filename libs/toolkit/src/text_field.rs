// SPDX-License-Identifier: MIT OR Apache-2.0
//! Single-line iced input with bounded, selection-aware undo history.
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use iced_core::widget::operation::{Focusable as _, TextInput as _};
use iced_core::{Element, Event, Length, Padding, Pixels, Rectangle, Size, keyboard, mouse};
use iced_core::{Layout, Shell, Widget, layout, renderer, text, widget};
mod input;
mod raw;
use iced_widget::text_input;
use raw::TextInput;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Selection {
    Index(usize),
    Selection { start: usize, end: usize },
}

impl From<text::editor::Cursor> for Selection {
    fn from(cursor: text::editor::Cursor) -> Self {
        match cursor.selection {
            Some(anchor) => Self::Selection {
                start: cursor.position.index,
                end: anchor.index,
            },
            None => Self::Index(cursor.position.index),
        }
    }
}

fn position(index: usize) -> text::Position {
    text::Position { line: 0, index }
}

#[derive(Clone)]
enum InputMessage {
    Changed(String),
    Submit,
}

#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    value: String,
    selection: Selection,
}

#[derive(Clone, Debug)]
struct Edit {
    before: Snapshot,
    after: Snapshot,
}

#[derive(Default)]
struct History {
    expected: String,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    typing_at: Option<Instant>,
    composing: bool,
    ime_blocked: bool,
    window_blurred: bool,
}

impl History {
    fn record(&mut self, before: Snapshot, after: Snapshot, typing: bool, now: Instant) {
        if before.value == after.value {
            return;
        }
        let coalesce = typing
            && self
                .typing_at
                .is_some_and(|at| now.duration_since(at) < Duration::from_secs(1))
            && self.undo.last().is_some_and(|edit| edit.after == before);
        self.expected.clone_from(&after.value);
        if coalesce {
            self.undo.last_mut().expect("existing typing group").after = after;
        } else {
            self.undo.push(Edit { before, after });
            if self.undo.len() > 100 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.typing_at = typing.then_some(now);
    }

    fn restore(&mut self, redo: bool) -> Option<Snapshot> {
        self.typing_at = None;
        let (source, destination) = if redo {
            (&mut self.redo, &mut self.undo)
        } else {
            (&mut self.undo, &mut self.redo)
        };
        let edit = source.pop()?;
        let result = if redo {
            edit.after.clone()
        } else {
            edit.before.clone()
        };
        self.expected.clone_from(&result.value);
        destination.push(edit);
        Some(result)
    }
}

/// A controlled input: store each `on_input` message's value in application state.
///
/// Text entry, selection, clipboard handling and IME remain iced's responsibility.
/// Adjacent non-whitespace typing within one second forms an undo group. Cursor
/// movement, paste, composition, whitespace and deletion end the group. Up to
/// 100 groups are retained; an external value replacement clears history.
pub struct TextField<'a, Message, Theme, Renderer>
where
    Theme: text_input::Catalog,
    Renderer: text::Renderer + 'static,
{
    input: TextInput<'a, InputMessage, Theme, Renderer>,
    value: String,
    on_input: Option<Box<dyn Fn(String) -> Message + 'a>>,
    on_submit: Option<Box<dyn Fn() -> Message + 'a>>,
    config: Config<'a, Theme>,
}

type InputStyle<'a, Theme> = dyn Fn(&Theme, text_input::Status) -> text_input::Style + 'a;

struct Config<'a, Theme> {
    placeholder: String,
    secure: bool,
    id: Option<widget::Id>,
    width: Option<Length>,
    padding: Option<Padding>,
    size: Option<Pixels>,
    style: Option<Rc<InputStyle<'a, Theme>>>,
}

impl<Theme> Default for Config<'_, Theme> {
    fn default() -> Self {
        Self {
            placeholder: String::new(),
            secure: false,
            id: None,
            width: None,
            padding: None,
            size: None,
            style: None,
        }
    }
}

impl<'a, Message, Theme, Renderer> TextField<'a, Message, Theme, Renderer>
where
    Theme: text_input::Catalog + 'a,
    Theme::Class<'a>: From<text_input::StyleFn<'a, Theme>>,
    Renderer: text::Renderer + 'static,
{
    /// Creates an input with a placeholder and the application's current value.
    pub fn new(placeholder: &str, value: &str) -> Self {
        Self {
            input: TextInput::new(placeholder.to_owned(), value.to_owned()),
            value: value.into(),
            on_input: None,
            on_submit: None,
            config: Config {
                placeholder: placeholder.into(),
                ..Config::default()
            },
        }
    }

    /// Enables editing and maps new values into application messages.
    pub fn on_input(mut self, callback: impl Fn(String) -> Message + 'a) -> Self {
        self.input = self.input.on_input(InputMessage::Changed);
        self.on_input = Some(Box::new(callback));
        self
    }

    /// Publishes a message when iced submits the focused input with Enter.
    pub fn on_submit(mut self, message: Message) -> Self
    where
        Message: Clone + 'a,
    {
        self.input = self.input.on_submit(InputMessage::Submit);
        self.on_submit = Some(Box::new(move || message.clone()));
        self
    }

    /// Uses iced's password masking and secure input-method purpose.
    pub fn secure(mut self, secure: bool) -> Self {
        self.config.secure = secure;
        self.input = self.input.secure(secure);
        self
    }

    /// Sets an ID for iced focus and selection operations.
    pub fn id(mut self, id: impl Into<widget::Id>) -> Self {
        let id = id.into();
        self.config.id = Some(id.clone());
        self.input = self.input.id(id);
        self
    }

    /// Sets the input width.
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        let width = width.into();
        self.config.width = Some(width);
        self.input = self.input.width(width);
        self
    }

    /// Sets the input padding.
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        let padding = padding.into();
        self.config.padding = Some(padding);
        self.input = self.input.padding(padding);
        self
    }

    /// Sets the text size.
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        let size = size.into();
        self.config.size = Some(size);
        self.input = self.input.size(size);
        self
    }

    /// A style closure over the theme. Without one the theme's default
    /// text-input class applies (for `toolkit::Theme`, `Tokens::text_input`).
    pub fn style(
        mut self,
        style: impl Fn(&Theme, text_input::Status) -> text_input::Style + 'a,
    ) -> Self {
        let style = Rc::new(style);
        self.config.style = Some(style.clone());
        self.input = self.input.style(move |theme, status| style(theme, status));
        self
    }

    // iced's TextInput has no value setter. Rebuild its configuration immediately
    // on undo so a second event in the same runtime batch sees the restored value.
    // Its widget tree (focus, selection, IME and paragraph state) is retained.
    fn restore_value(&mut self, value: String) {
        self.value = value;
        let config = &self.config;
        let mut input =
            TextInput::new(config.placeholder.clone(), self.value.clone()).secure(config.secure);
        if self.on_input.is_some() {
            input = input.on_input(InputMessage::Changed);
        }
        if self.on_submit.is_some() {
            input = input.on_submit(InputMessage::Submit);
        }
        if let Some(id) = &config.id {
            input = input.id(id.clone());
        }
        if let Some(width) = config.width {
            input = input.width(width);
        }
        if let Some(padding) = config.padding {
            input = input.padding(padding);
        }
        if let Some(size) = config.size {
            input = input.size(size);
        }
        if let Some(style) = &config.style {
            let style = style.clone();
            input = input.style(move |theme, status| style(theme, status));
        }
        self.input = input;
    }
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for TextField<'a, Message, Theme, Renderer>
where
    Theme: text_input::Catalog + 'a,
    Theme::Class<'a>: From<text_input::StyleFn<'a, Theme>>,
    Renderer: text::Renderer + 'static,
{
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<History>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(History {
            expected: self.value.clone(),
            ..History::default()
        })
    }

    fn diff(&mut self, tree: &mut widget::Tree) {
        if tree.children.is_empty() {
            tree.children.push(widget::Tree::new(
                &self.input as &dyn Widget<InputMessage, Theme, Renderer>,
            ));
        }
        self.input.diff(&mut tree.children[0]);
        let history = tree.state.downcast_mut::<History>();
        if history.expected != self.value {
            *history = History {
                expected: self.value.clone(),
                composing: history.composing,
                ime_blocked: history.ime_blocked,
                window_blurred: history.window_blurred,
                ..History::default()
            };
        }
    }

    fn size(&self) -> Size<Length> {
        Widget::size(&self.input)
    }

    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        Widget::layout(&mut self.input, &mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut widget::Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.input
            .operate(&mut tree.children[0], layout, renderer, operation);
        if tree.state.downcast_ref::<History>().composing
            && !tree.children[0]
                .state
                .downcast_ref::<raw::State<Renderer>>()
                .is_focused()
        {
            // A modal can suspend this subtree before the IME sends Closed.
            // Cancel only preedit; committed text, selection and undo survive.
            let mut bus = iced_core::shell::Bus::new();
            let mut shell = Shell::new(
                &iced_core::window::Headless,
                iced_core::shell::Waker::noop(),
                &mut bus,
            );
            self.input.update(
                &mut tree.children[0],
                &Event::InputMethod(iced_core::input_method::Event::Closed),
                layout,
                mouse::Cursor::Unavailable,
                renderer,
                &mut shell,
                &layout.bounds(),
            );
            let history = tree.state.downcast_mut::<History>();
            history.composing = false;
            history.ime_blocked = true;
        }
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let history = tree.state.downcast_mut::<History>();
        if matches!(
            event,
            Event::InputMethod(iced_core::input_method::Event::Closed)
        ) {
            history.ime_blocked = false;
        } else if history.ime_blocked && matches!(event, Event::InputMethod(_)) {
            // Local suspension clears presentation immediately, but only the
            // runtime acknowledgement permits a new composition epoch.
            return;
        }
        let child = &mut tree.children[0];
        let state = child.state.downcast_mut::<raw::State<Renderer>>();
        let selection = Selection::from(state.cursor());
        // iced 0.15 now has native undo bindings. Keep this wrapper's history
        // authoritative, including while the window is blurred or IME is active.
        if let Event::Keyboard(keyboard::Event::KeyPressed {
            key,
            physical_key,
            modifiers,
            ..
        }) = event
            && modifiers.control()
            && !modifiers.alt()
            && !modifiers.logo()
            && key
                .to_latin(*physical_key)
                .is_some_and(|key| matches!(key.to_ascii_lowercase(), 'z' | 'y'))
            && (!state.is_focused()
                || history.window_blurred
                || history.composing
                || self.on_input.is_none())
        {
            return;
        }
        if state.is_focused()
            && !history.window_blurred
            && self.on_input.is_some()
            && !history.composing
            && let Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                ..
            }) = event
            && modifiers.control()
            && !modifiers.alt()
            && !modifiers.logo()
            // Match iced's clipboard shortcuts across non-Latin layouts while
            // respecting logical keys on remapped Latin layouts.
            && let Some(shortcut) = key.to_latin(*physical_key)
            && matches!(shortcut.to_ascii_lowercase(), 'z' | 'y')
        {
            if let Some(snapshot) =
                history.restore(shortcut.eq_ignore_ascii_case(&'y') || modifiers.shift())
            {
                state.overwrite(&snapshot.value);
                match snapshot.selection {
                    Selection::Index(index) => state.move_cursor_to(position(index)),
                    Selection::Selection { start, end } => {
                        state.select_range(position(start), position(end))
                    }
                }
                self.restore_value(snapshot.value);
                // Refresh paragraph caches before any further event in this batch.
                let bounds = layout.bounds().size();
                Widget::layout(
                    &mut self.input,
                    child,
                    renderer,
                    &layout::Limits::new(bounds, bounds),
                );
                shell.publish(self.on_input.as_ref().expect("enabled input")(
                    self.value.clone(),
                ));
                shell.invalidate_widgets();
                shell.request_redraw();
            }
            shell.capture_event();
            return;
        }

        let typing = matches!(event,
            Event::Keyboard(keyboard::Event::KeyPressed { text: Some(text), modifiers, .. })
            if !modifiers.control() && !modifiers.alt() && !modifiers.logo()
                && text.chars().count() == 1 && !text.chars().any(char::is_whitespace))
            && matches!(selection, Selection::Index(_))
            && !history.composing;
        match event {
            Event::InputMethod(iced_core::input_method::Event::Preedit(text, _))
                if state.is_focused() =>
            {
                history.composing = !text.is_empty()
            }
            Event::InputMethod(
                iced_core::input_method::Event::Commit(_) | iced_core::input_method::Event::Closed,
            ) => history.composing = false,
            Event::Window(iced_core::window::Event::Unfocused) => history.window_blurred = true,
            Event::Window(iced_core::window::Event::Focused) => history.window_blurred = false,
            _ => {}
        }
        // Redraws and modifier releases must not split an otherwise contiguous group.
        if !typing
            && matches!(
                event,
                Event::Keyboard(keyboard::Event::KeyPressed { .. })
                    | Event::Mouse(mouse::Event::ButtonPressed(_))
                    | Event::Touch(_)
                    | Event::InputMethod(_)
                    | Event::Clipboard(_)
                    | Event::Window(iced_core::window::Event::Unfocused)
            )
        {
            history.typing_at = None;
        }
        let mut messages = iced_core::shell::Bus::new();
        let mut inner_shell = shell.local(&mut messages);
        self.input.update(
            child,
            event,
            layout,
            cursor,
            renderer,
            &mut inner_shell,
            viewport,
        );
        let input_state = child.state.downcast_ref::<raw::State<Renderer>>();
        let previous_value = self.value.clone();
        let history = RefCell::new(history);
        let value = RefCell::new(&mut self.value);
        shell.merge(inner_shell, |next| {
            let next = match next {
                InputMessage::Changed(next) => next,
                InputMessage::Submit => return self.on_submit.as_ref().expect("submit enabled")(),
            };
            let after = Snapshot {
                selection: Selection::from(input_state.cursor()),
                value: next.clone(),
            };
            let before = Snapshot {
                value: (**value.borrow()).clone(),
                selection,
            };
            history
                .borrow_mut()
                .record(before, after, typing, Instant::now());
            **value.borrow_mut() = next.clone();
            self.on_input
                .as_ref()
                .expect("iced only emits for enabled input")(next)
        });
        if self.value != previous_value {
            // 0.15 stores the live edit in its tree while the widget retains a
            // controlled fragment. Keep that fragment current for another
            // event/layout before the host reconstructs the wrapper.
            self.restore_value(self.value.clone());
        }
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        Widget::draw(
            &self.input,
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.input
            .mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }
}

impl<'a, Message, Theme, Renderer> From<TextField<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: text_input::Catalog + 'a,
    Theme::Class<'a>: From<text_input::StyleFn<'a, Theme>>,
    Renderer: text::Renderer + 'static + 'a,
{
    fn from(input: TextField<'a, Message, Theme, Renderer>) -> Self {
        Element::new(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(value: &str, cursor: usize) -> Snapshot {
        Snapshot {
            value: value.into(),
            selection: Selection::Index(cursor),
        }
    }

    #[test]
    fn adjacent_typing_coalesces_and_redoes() {
        let mut history = History::default();
        let now = Instant::now();
        history.record(snap("", 0), snap("a", 1), true, now);
        history.record(
            snap("a", 1),
            snap("ab", 2),
            true,
            now + Duration::from_millis(100),
        );
        assert_eq!(history.undo.len(), 1);
        assert_eq!(history.restore(false), Some(snap("", 0)));
        assert_eq!(history.restore(true), Some(snap("ab", 2)));
    }

    #[test]
    fn pause_and_cursor_movement_split_groups() {
        let mut history = History::default();
        let now = Instant::now();
        history.record(snap("", 0), snap("a", 1), true, now);
        history.record(
            snap("a", 1),
            snap("ab", 2),
            true,
            now + Duration::from_secs(2),
        );
        history.record(
            snap("ab", 0),
            snap("cab", 1),
            true,
            now + Duration::from_millis(2100),
        );
        assert_eq!(history.undo.len(), 3);
    }

    #[test]
    fn selection_and_composition_are_atomic_and_new_edits_clear_redo() {
        let before = Snapshot {
            value: "abcd".into(),
            selection: Selection::Selection { start: 3, end: 1 },
        };
        let mut history = History::default();
        let now = Instant::now();
        history.record(before.clone(), snap("a界d", 2), false, now);
        assert_eq!(history.restore(false), Some(before.clone()));
        history.record(before, snap("ad", 1), false, now);
        assert_eq!(history.restore(true), None);
    }

    #[test]
    fn history_is_bounded_and_noops_do_not_clear_redo() {
        let mut history = History::default();
        let now = Instant::now();
        for n in 0..110 {
            history.record(
                snap(&n.to_string(), 0),
                snap(&(n + 1).to_string(), 0),
                false,
                now,
            );
        }
        assert_eq!(history.undo.len(), 100);
        history.restore(false);
        history.record(snap("109", 0), snap("109", 0), false, now);
        assert!(history.restore(true).is_some());
    }
}

// iced provides a no-op text renderer in debug builds. These tests exercise the
// real TextInput update path without creating a window, GPU or application shell.
#[cfg(all(test, debug_assertions))]
mod widget_tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;
    use iced_core::input_method;

    type Theme = iced_core::Theme;
    type Field = TextField<'static, String, Theme, LayoutRenderer>;

    fn field(value: &str) -> (Field, widget::Tree) {
        let mut field = TextField::new("placeholder", value).on_input(std::convert::identity);
        let mut tree = widget::Tree::new(&field as &dyn Widget<String, Theme, LayoutRenderer>);
        field.diff(&mut tree);
        let bounds = Size::new(300.0, 40.0);
        Widget::layout(
            &mut field,
            &mut tree,
            &LayoutRenderer::new(),
            &layout::Limits::new(bounds, bounds),
        );
        tree.children[0]
            .state
            .downcast_mut::<raw::State<LayoutRenderer>>()
            .focus();
        (field, tree)
    }

    fn send(
        field: &mut Field,
        tree: &mut widget::Tree,
        event: Event,
    ) -> (Vec<String>, input_method::InputMethod) {
        let bounds = Size::new(300.0, 40.0);
        let node = Widget::layout(
            field,
            tree,
            &LayoutRenderer::new(),
            &layout::Limits::new(bounds, bounds),
        );
        let mut messages = iced_core::shell::Bus::new();
        let mut shell = Shell::new(
            &iced_core::window::Headless,
            iced_core::shell::Waker::noop(),
            &mut messages,
        );
        field.update(
            tree,
            &event,
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &LayoutRenderer::new(),
            &mut shell,
            &Rectangle::with_size(bounds),
        );
        let ime = shell.input_method().clone();
        (messages.into_iter().collect(), ime)
    }

    fn key(character: &str, modifiers: keyboard::Modifiers) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Character(character.into()),
            modified_key: keyboard::Key::Character(character.into()),
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers,
            text: (!modifiers.control()).then(|| character.into()),
            repeat: false,
        })
    }

    #[test]
    fn clipboard_request_is_forwarded_and_delivery_is_one_undo_group() {
        use iced_core::clipboard;
        use std::sync::Arc;
        let (mut field, mut tree) = field("");
        send(
            &mut field,
            &mut tree,
            key("a", keyboard::Modifiers::empty()),
        );
        let bounds = Size::new(300.0, 40.0);
        let renderer = LayoutRenderer::new();
        let node = Widget::layout(
            &mut field,
            &mut tree,
            &renderer,
            &layout::Limits::new(bounds, bounds),
        );
        let mut bus = iced_core::shell::Bus::new();
        let mut shell = Shell::new(
            &iced_core::window::Headless,
            iced_core::shell::Waker::noop(),
            &mut bus,
        );
        field.update(
            &mut tree,
            &key("v", keyboard::Modifiers::CTRL),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &renderer,
            &mut shell,
            &Rectangle::with_size(bounds),
        );
        assert_eq!(shell.clipboard_mut().reads, [clipboard::Kind::Text]);
        assert!(shell.is_event_captured());
        assert!(shell.is_empty());
        let pasted = Event::Clipboard(clipboard::Event::Read(Ok(Arc::new(
            clipboard::Content::Text("界b".into()),
        ))));
        assert_eq!(send(&mut field, &mut tree, pasted).0, ["a界b"]);
        assert_eq!(
            send(
                &mut field,
                &mut tree,
                key("c", keyboard::Modifiers::empty())
            )
            .0,
            ["a界bc"]
        );
        assert_eq!(
            send(&mut field, &mut tree, key("z", keyboard::Modifiers::CTRL)).0,
            ["a界b"]
        );
        assert_eq!(
            send(&mut field, &mut tree, key("z", keyboard::Modifiers::CTRL)).0,
            ["a"]
        );
        assert_eq!(
            send(&mut field, &mut tree, key("y", keyboard::Modifiers::CTRL)).0,
            ["a界b"]
        );
    }

    #[test]
    fn secure_unicode_selection_survives_undo_and_following_typing() {
        let (field, mut tree) = field("界ab");
        let mut field = field.secure(true);
        // Layout activates masking before setting the original-text byte range.
        let bounds = Size::new(300.0, 40.0);
        Widget::layout(
            &mut field,
            &mut tree,
            &LayoutRenderer::new(),
            &layout::Limits::new(bounds, bounds),
        );
        tree.children[0]
            .state
            .downcast_mut::<raw::State<LayoutRenderer>>()
            .select_range(position(3), position(0));
        assert_eq!(
            send(
                &mut field,
                &mut tree,
                key("c", keyboard::Modifiers::empty())
            )
            .0,
            ["cab"]
        );
        assert_eq!(
            send(&mut field, &mut tree, key("z", keyboard::Modifiers::CTRL)).0,
            ["界ab"]
        );
        assert_eq!(
            send(
                &mut field,
                &mut tree,
                key("d", keyboard::Modifiers::empty())
            )
            .0,
            ["dab"]
        );
    }

    #[test]
    fn submit_is_distinct_from_edits_and_survives_undo() {
        let (field, mut tree) = field("path");
        let mut field = field.on_submit("submitted".to_owned());
        send(
            &mut field,
            &mut tree,
            key("a", keyboard::Modifiers::empty()),
        );
        send(&mut field, &mut tree, key("z", keyboard::Modifiers::CTRL));
        let mut enter = key("", keyboard::Modifiers::empty());
        if let Event::Keyboard(keyboard::Event::KeyPressed {
            key,
            modified_key,
            text,
            ..
        }) = &mut enter
        {
            *key = keyboard::Key::Named(keyboard::key::Named::Enter);
            *modified_key = key.clone();
            *text = None;
        }
        assert_eq!(send(&mut field, &mut tree, enter.clone()).0, ["submitted"]);
        assert_eq!(field.value, "path");
        tree.children[0]
            .state
            .downcast_mut::<raw::State<LayoutRenderer>>()
            .unfocus();
        assert!(send(&mut field, &mut tree, enter).0.is_empty());
    }

    #[test]
    fn undo_then_typing_in_same_widget_instance_uses_restored_value() {
        let (mut field, mut tree) = field("");
        assert_eq!(
            send(
                &mut field,
                &mut tree,
                key("a", keyboard::Modifiers::empty())
            )
            .0,
            ["a"]
        );
        assert_eq!(
            send(
                &mut field,
                &mut tree,
                key("b", keyboard::Modifiers::empty())
            )
            .0,
            ["ab"]
        );
        assert_eq!(
            send(&mut field, &mut tree, key("z", keyboard::Modifiers::CTRL)).0,
            [""]
        );
        assert_eq!(
            send(
                &mut field,
                &mut tree,
                key("c", keyboard::Modifiers::empty())
            )
            .0,
            ["c"]
        );
        assert!(
            send(&mut field, &mut tree, key("y", keyboard::Modifiers::CTRL))
                .0
                .is_empty()
        );
    }

    #[test]
    fn non_latin_layout_undo_and_both_redo_shortcuts_use_physical_keys() {
        let (mut field, mut tree) = field("");
        send(
            &mut field,
            &mut tree,
            key("a", keyboard::Modifiers::empty()),
        );
        let physical_key = |character: &str, code, modifiers| {
            let mut event = key(character, modifiers);
            if let Event::Keyboard(keyboard::Event::KeyPressed { physical_key, .. }) = &mut event {
                *physical_key = keyboard::key::Physical::Code(code);
            }
            event
        };
        let undo = || physical_key("я", keyboard::key::Code::KeyZ, keyboard::Modifiers::CTRL);
        assert_eq!(send(&mut field, &mut tree, undo()).0, [""]);
        assert_eq!(
            send(
                &mut field,
                &mut tree,
                physical_key(
                    "Я",
                    keyboard::key::Code::KeyZ,
                    keyboard::Modifiers::CTRL | keyboard::Modifiers::SHIFT
                )
            )
            .0,
            ["a"]
        );
        assert_eq!(send(&mut field, &mut tree, undo()).0, [""]);
        assert_eq!(
            send(
                &mut field,
                &mut tree,
                physical_key("υ", keyboard::key::Code::KeyY, keyboard::Modifiers::CTRL)
            )
            .0,
            ["a"]
        );
        // A remapped Latin logical key takes precedence over physical position,
        // matching iced's native clipboard policy.
        assert!(
            send(
                &mut field,
                &mut tree,
                physical_key("q", keyboard::key::Code::KeyZ, keyboard::Modifiers::CTRL)
            )
            .0
            .is_empty()
        );
    }

    #[test]
    fn ime_closed_clears_composition_and_reenables_undo() {
        let (mut field, mut tree) = field("");
        send(
            &mut field,
            &mut tree,
            key("a", keyboard::Modifiers::empty()),
        );
        send(
            &mut field,
            &mut tree,
            Event::InputMethod(input_method::Event::Preedit("界".into(), Some(0..3))),
        );
        assert!(
            send(&mut field, &mut tree, key("z", keyboard::Modifiers::CTRL))
                .0
                .is_empty()
        );
        send(
            &mut field,
            &mut tree,
            Event::InputMethod(input_method::Event::Closed),
        );
        let (_, ime) = send(
            &mut field,
            &mut tree,
            Event::Window(iced_core::window::Event::RedrawRequested(Instant::now())),
        );
        assert!(matches!(
            ime,
            input_method::InputMethod::Enabled { preedit: None, .. }
        ));
        assert_eq!(
            send(&mut field, &mut tree, key("z", keyboard::Modifiers::CTRL)).0,
            [""]
        );
    }

    #[test]
    fn ime_preedit_is_forwarded_commit_is_atomic_and_undo_restores_selection() {
        let (mut field, mut tree) = field("abcd");
        tree.children[0]
            .state
            .downcast_mut::<raw::State<LayoutRenderer>>()
            .select_range(position(3), position(1));
        assert!(
            send(
                &mut field,
                &mut tree,
                Event::InputMethod(input_method::Event::Preedit("界".into(), Some(0..3)))
            )
            .0
            .is_empty()
        );
        let (_, ime) = send(
            &mut field,
            &mut tree,
            Event::Window(iced_core::window::Event::RedrawRequested(Instant::now())),
        );
        assert!(
            matches!(ime, input_method::InputMethod::Enabled { preedit: Some(preedit), .. } if preedit.content == "界")
        );
        assert!(
            send(&mut field, &mut tree, key("z", keyboard::Modifiers::CTRL))
                .0
                .is_empty()
        );
        send(
            &mut field,
            &mut tree,
            Event::InputMethod(input_method::Event::Preedit(String::new(), None)),
        );
        assert_eq!(
            send(
                &mut field,
                &mut tree,
                Event::InputMethod(input_method::Event::Commit("界".into()))
            )
            .0,
            ["a界d"]
        );
        assert_eq!(
            send(&mut field, &mut tree, key("z", keyboard::Modifiers::CTRL)).0,
            ["abcd"]
        );
        assert_eq!(
            Selection::from(
                tree.children[0]
                    .state
                    .downcast_ref::<raw::State<LayoutRenderer>>()
                    .cursor()
            ),
            Selection::Selection { start: 3, end: 1 }
        );
        assert_eq!(
            send(
                &mut field,
                &mut tree,
                key("z", keyboard::Modifiers::CTRL | keyboard::Modifiers::SHIFT)
            )
            .0,
            ["a界d"]
        );
    }

    #[test]
    fn external_replacement_keeps_live_composition_and_clears_old_history() {
        let (mut old, mut tree) = field("");
        send(&mut old, &mut tree, key("a", keyboard::Modifiers::empty()));
        send(
            &mut old,
            &mut tree,
            Event::InputMethod(input_method::Event::Preedit("界".into(), Some(0..3))),
        );
        let (mut replacement, _) = field("new");
        replacement.diff(&mut tree);
        let history = tree.state.downcast_ref::<History>();
        assert!(history.composing);
        assert!(history.undo.is_empty());
        let (_, ime) = send(
            &mut replacement,
            &mut tree,
            Event::Window(iced_core::window::Event::RedrawRequested(Instant::now())),
        );
        assert!(
            matches!(ime, input_method::InputMethod::Enabled { preedit: Some(preedit), .. } if preedit.content == "界")
        );
        assert_eq!(
            send(
                &mut replacement,
                &mut tree,
                Event::InputMethod(input_method::Event::Commit("界".into()))
            )
            .0,
            ["n界ew"]
        );
        assert_eq!(
            send(
                &mut replacement,
                &mut tree,
                key("z", keyboard::Modifiers::CTRL)
            )
            .0,
            ["new"]
        );
        assert!(
            send(
                &mut replacement,
                &mut tree,
                key("z", keyboard::Modifiers::CTRL)
            )
            .0
            .is_empty()
        );
    }

    #[test]
    fn unfocused_preedit_and_window_blur_do_not_steal_undo() {
        let (mut field, mut tree) = field("");
        send(
            &mut field,
            &mut tree,
            key("a", keyboard::Modifiers::empty()),
        );
        send(
            &mut field,
            &mut tree,
            Event::Window(iced_core::window::Event::Unfocused),
        );
        assert!(
            send(&mut field, &mut tree, key("z", keyboard::Modifiers::CTRL))
                .0
                .is_empty()
        );
        send(
            &mut field,
            &mut tree,
            Event::Window(iced_core::window::Event::Focused),
        );
        tree.children[0]
            .state
            .downcast_mut::<raw::State<LayoutRenderer>>()
            .unfocus();
        send(
            &mut field,
            &mut tree,
            Event::InputMethod(input_method::Event::Preedit("界".into(), None)),
        );
        assert!(!tree.state.downcast_ref::<History>().composing);
        tree.children[0]
            .state
            .downcast_mut::<raw::State<LayoutRenderer>>()
            .focus();
        assert_eq!(
            send(&mut field, &mut tree, key("z", keyboard::Modifiers::CTRL)).0,
            [""]
        );
    }
}
