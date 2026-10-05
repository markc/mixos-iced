// SPDX-License-Identifier: MIT
// Adapted from iced 3de451447, core/src/text/input.rs and widget/src/text_input.rs.
/*
Copyright 2019 Héctor Ramón, Iced contributors

Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software is furnished to do so,
subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS
FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
*/
#![allow(missing_docs)] // TODO
use iced_core::alignment;
use iced_core::clipboard;
use iced_core::layout;
use iced_core::mouse;
use iced_core::text::editor;
use iced_core::text::paragraph;
use iced_core::text::{self, Alignment, Editor, LineHeight, Position, Text, Wrapping};
use iced_core::widget::operation::{Focusable, TextInput};
use iced_core::{Color, Event, InputMethod, Length, Padding, Pixels, Point, Rectangle, Shell};

use unicode_segmentation::UnicodeSegmentation;

use std::sync::Arc;

const SECURE_CHAR: char = '•';

pub struct Input<R: text::Renderer> {
    editor: R::Editor,
    secure: Option<R::Editor>,
    state: editor::State,
    placeholder: paragraph::Plain<R::Paragraph>,
    padding: Padding,
    multiline: Option<Wrapping>,
}

pub struct Layout<'a, Font> {
    pub width: Length,
    pub height: Length,
    pub padding: Padding,
    pub placeholder: &'a str,
    pub font: Option<Font>,
    pub size: Option<Pixels>,
    pub line_height: LineHeight,
    pub alignment: Alignment,
    pub multiline: Option<Wrapping>,
    pub is_secure: bool,
}

impl<R: text::Renderer> Input<R> {
    pub fn new() -> Self {
        Self {
            editor: R::Editor::with_text(""),
            secure: None,
            state: editor::State::new(),
            placeholder: paragraph::Plain::default(),
            padding: Padding::default(),
            multiline: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.editor.is_empty()
    }

    pub fn value(&self) -> String {
        Editor::text(&self.editor)
    }

    /// Returns the cursor and selection anchor in the unmasked text.
    pub fn cursor(&self) -> editor::Cursor {
        self.editor.cursor()
    }

    pub fn placeholder(&self) -> &str {
        self.placeholder.content()
    }

    pub fn overwrite(&mut self, value: &str) {
        self.editor.overwrite(value);

        if let Some(secure) = &mut self.secure {
            let secured = protect(value, self.multiline.is_some());
            secure.overwrite(&secured);
        }
        self.sync_secure_cursor();
    }

    fn sync_secure_cursor(&mut self) {
        fn position(editor: &impl Editor, position: Position) -> Position {
            let index = editor.line(position.line).map_or(0, |line| {
                line.text
                    .grapheme_indices(true)
                    .take_while(|(at, _)| *at < position.index)
                    .count()
                    * SECURE_CHAR.len_utf8()
            });
            Position {
                line: position.line,
                index,
            }
        }
        if let Some(secure) = &mut self.secure {
            let cursor = self.editor.cursor();
            secure.move_to(editor::Cursor {
                position: position(&self.editor, cursor.position),
                selection: cursor
                    .selection
                    .map(|anchor| position(&self.editor, anchor)),
            });
        }
    }

    pub fn layout(
        &mut self,
        renderer: &R,
        limits: &layout::Limits,
        layout: Layout<'_, R::Font>,
    ) -> layout::Node {
        self.padding = layout.padding;
        self.multiline = layout.multiline;

        let limits = limits
            .width(layout.width)
            .height(layout.height)
            .shrink(layout.padding);

        let font = layout.font.unwrap_or_else(|| renderer.default_font());
        let size = layout.size.unwrap_or_else(|| renderer.default_size());
        let hint_factor = renderer.hint_factor();

        if layout.is_secure {
            if self.secure.is_none() {
                let value = self.value();
                let secured = protect(&value, layout.multiline.is_some());

                self.secure = Some(text::Editor::with_text(&secured));
                self.sync_secure_cursor();
            }
        } else {
            self.secure = None;
        }

        let editor = self.secure.as_mut().unwrap_or(&mut self.editor);

        editor.update(
            limits.max(),
            font,
            size,
            layout.line_height,
            layout.multiline.unwrap_or(text::Wrapping::None),
            layout.alignment,
            hint_factor,
            &mut text::highlighter::PlainText,
        );

        let bounds = match layout.height {
            Length::Fill
            | Length::FillPortion(_)
            | Length::Fixed(_)
            | Length::Bounded { .. }
            | Length::Fluid(_) => limits.max(),
            Length::Shrink | Length::Fit => {
                limits.resolve(layout.width, layout.height, editor.min_bounds())
            }
        };

        let _ = self.placeholder.update(Text {
            content: layout.placeholder,
            font,
            line_height: layout.line_height,
            bounds,
            size,
            align_x: layout.alignment,
            align_y: alignment::Vertical::Top,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::None,
            ellipsis: text::Ellipsis::None,
            hint_factor,
        });

        layout::Node::new(bounds.expand(layout.padding))
    }

    pub fn update<Message>(
        &mut self,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
        shell: &mut Shell<'_, Message>,
        key_binding: impl Fn(editor::KeyPress) -> Option<editor::Binding<Message>>,
    ) -> Option<Edit> {
        fn apply<Message>(
            editor: &mut impl Editor,
            shell: &mut Shell<'_, Message>,
            update: editor::Update<Message>,
            is_multiline: bool,
        ) -> Option<Edit> {
            match update {
                editor::Update::Action(action) => {
                    let (action, is_paste) = match action {
                        editor::Action::Edit(editor::Edit::Enter)
                        | editor::Action::Move(editor::Motion::Up)
                        | editor::Action::Move(editor::Motion::Down)
                            if !is_multiline =>
                        {
                            return None;
                        }
                        editor::Action::Edit(editor::Edit::Paste(text)) => (
                            editor::Action::Edit(editor::Edit::Paste(if !is_multiline {
                                Arc::new(text.lines().collect())
                            } else {
                                text
                            })),
                            true,
                        ),
                        _ => (action, false),
                    };

                    let is_edit = action.is_edit();

                    editor.perform(action);
                    shell.capture_event();

                    if is_edit && is_multiline {
                        shell.invalidate_layout();
                    } else {
                        shell.request_redraw();
                    }

                    return is_edit.then_some(Edit {
                        has_pasted: is_paste,
                    });
                }
                editor::Update::Focus | editor::Update::InputMethod => {
                    shell.request_redraw();
                    shell.capture_event();
                }
                editor::Update::Unfocus => {
                    shell.request_redraw();
                }
                editor::Update::Release => {}
                editor::Update::Copy(text) => {
                    shell.write_clipboard(text);
                    shell.capture_event();
                }
                editor::Update::Paste => {
                    shell.read_clipboard(clipboard::Kind::Text);
                    shell.capture_event();
                }
                editor::Update::RedrawAt(at) => {
                    shell.request_redraw_at(at);
                }
                editor::Update::Custom(message) => {
                    shell.publish(message);
                    shell.capture_event();
                }
                editor::Update::Sequence(updates) => {
                    let mut edit: Option<Edit> = None;

                    for update in updates {
                        if let Some(new_edit) = apply(editor, shell, update, is_multiline) {
                            edit = Some(Edit {
                                has_pasted: edit.unwrap_or_default().has_pasted
                                    || new_edit.has_pasted,
                            });
                        }
                    }

                    return edit;
                }
            }

            None
        }

        let editor = self.secure.as_ref().unwrap_or(&self.editor);

        let update = self
            .state
            .update(editor, event, bounds, self.padding, cursor, key_binding)?;

        if let Some(secure) = &mut self.secure {
            fn apply_secure<Message>(
                editor: &mut impl Editor,
                update: &editor::Update<Message>,
                is_multiline: bool,
            ) {
                match update {
                    editor::Update::Action(action) => {
                        let action = match action {
                            editor::Action::Edit(editor::Edit::Insert(_)) => {
                                editor::Action::Edit(editor::Edit::Insert(SECURE_CHAR))
                            }
                            editor::Action::Edit(editor::Edit::Paste(text)) => {
                                let text = protect(text, is_multiline);

                                editor::Action::Edit(editor::Edit::Paste(Arc::new(text)))
                            }
                            action => action.clone(),
                        };

                        editor.perform(action);
                    }
                    editor::Update::Sequence(updates) => {
                        for update in updates {
                            apply_secure(editor, update, is_multiline);
                        }
                    }
                    _ => {}
                }
            }

            apply_secure(secure, &update, self.multiline.is_some());

            match &update {
                editor::Update::Action(action) if !action.is_edit() => {
                    fn translate(
                        editor: &impl text::Editor,
                        position: text::Position,
                    ) -> text::Position {
                        let Some(line) = editor.line(position.line) else {
                            return text::Position { line: 0, index: 0 };
                        };

                        let grapheme = position.index / SECURE_CHAR.len_utf8();
                        let index = line.text.graphemes(true).take(grapheme).map(str::len).sum();

                        text::Position {
                            line: position.line,
                            index,
                        }
                    }

                    let cursor = secure.cursor();

                    self.editor.move_to(editor::Cursor {
                        position: translate(&self.editor, cursor.position),
                        selection: cursor
                            .selection
                            .map(|selection| translate(&self.editor, selection)),
                    });

                    shell.request_redraw();

                    None
                }
                _ => apply(&mut self.editor, shell, update, self.multiline.is_some()),
            }
        } else {
            apply(&mut self.editor, shell, update, self.multiline.is_some())
        }
    }

    pub fn draw(&self, renderer: &mut R, bounds: Rectangle, viewport: Rectangle, style: Style) {
        let text_bounds = bounds.shrink(self.padding);

        let Some(clip_bounds) = text_bounds.intersection(&viewport) else {
            return;
        };

        let editor = self.secure.as_ref().unwrap_or(&self.editor);

        if editor.is_empty() {
            let anchor = text_bounds.anchor(
                self.placeholder.min_bounds(),
                self.placeholder.align_x(),
                self.placeholder.align_y(),
            );

            renderer.fill_paragraph(
                self.placeholder.raw(),
                anchor,
                style.placeholder,
                clip_bounds,
            );
        }

        self.state.draw(
            editor,
            renderer,
            text_bounds.position(),
            clip_bounds,
            editor::Style {
                value: style.value,
                selection: style.selection,
            },
        );
    }

    pub fn input_method(&self, position: Point) -> InputMethod<&str> {
        let mut method = self
            .state
            .input_method(self.secure.as_ref().unwrap_or(&self.editor), position);
        if self.secure.is_some()
            && let InputMethod::Enabled { purpose, .. } = &mut method
        {
            *purpose = iced_core::input_method::Purpose::Secure;
        }
        method
    }
}

impl<R: text::Renderer> Default for Input<R> {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Style {
    pub value: Color,
    pub selection: Color,
    pub placeholder: Color,
}

impl<R: text::Renderer> Focusable for Input<R> {
    fn is_focused(&self) -> bool {
        self.state.is_focused()
    }

    fn focus(&mut self) {
        self.state.focus();
    }

    fn unfocus(&mut self) {
        self.state.unfocus();
    }
}

impl<R: text::Renderer> TextInput for Input<R> {
    fn text(&self) -> text::Fragment<'_> {
        if self.editor.is_empty() {
            text::Fragment::Borrowed(self.placeholder.content())
        } else {
            TextInput::text(&self.editor)
        }
    }

    fn move_cursor_to(&mut self, position: Position) {
        self.editor.move_cursor_to(position);
        self.sync_secure_cursor();
    }

    fn move_cursor_to_front(&mut self) {
        self.editor.move_cursor_to_front();
        self.sync_secure_cursor();
    }

    fn move_cursor_to_end(&mut self) {
        self.editor.move_cursor_to_end();
        self.sync_secure_cursor();
    }

    fn select_all(&mut self) {
        self.editor.select_all();
        self.sync_secure_cursor();
    }

    fn select_range(&mut self, start: Position, end: Position) {
        self.editor.select_range(start, end);
        self.sync_secure_cursor();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Edit {
    pub has_pasted: bool,
}

fn protect(text: &str, is_multiline: bool) -> String {
    if is_multiline {
        text.lines()
            .map(|line| line.graphemes(true).map(|_| SECURE_CHAR).collect())
            .collect::<Vec<String>>()
            .join("\n")
    } else {
        text.graphemes(true).map(|_| SECURE_CHAR).collect()
    }
}
