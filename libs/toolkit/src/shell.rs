// SPDX-License-Identifier: MIT OR Apache-2.0
//! The application shell: [`Shell`] composes the chrome every desktop
//! app shares — a [`menu`](crate::menu) bar, a [`Toolbar`], sidebars
//! split from the content, and a [`StatusBar`] — so an app hands over
//! its menu items, tools, sidebar entries, content and status fields and
//! gets a window layout back.
//!
//! The parts are plain data plus one view call each; nothing here holds
//! state. Metrics are read when the view is built (apps rebuild their
//! view every update); colours resolve against the *live* theme when
//! the frame draws, so a token swap restyles the shell. The sample app
//! (`examples/shell.rs`, snapshotted by `tests/snapshots.rs`) is built
//! only from this module plus the theme.

use iced_core::{Element, Length, Padding};
use iced_widget::{Scrollable, button, column, container, row, text};

use crate::icon::Icon;
use crate::menu::{self, Item};
use crate::split::{self, Split};
use crate::theme::{self, Theme};
use crate::tokens::Tokens;

/// The app's concrete theme and renderer: the shell is an app-level
/// composition, so it fixes them rather than carrying them as
/// parameters. The renderer is whatever the `iced` umbrella provides.
type Renderer = iced_widget::Renderer;

/// The muted strip background every chrome piece uses, from the live
/// theme.
fn strip(theme: &Theme) -> iced_widget::container::Style {
    iced_widget::container::Style {
        background: Some(iced_core::Background::Color(
            theme.tokens().palette.muted_surface,
        )),
        ..iced_widget::container::Style::default()
    }
}

/// One toolbar control: a label, an optional icon, and the message it
/// produces. Disabled tools retain their message so they can be enabled again.
#[must_use]
pub struct Tool<Message> {
    label: String,
    icon: Option<Icon>,
    on_press: Message,
    enabled: bool,
    toggled: bool,
}

/// Creates an enabled [`Tool`] with a label and message.
pub fn tool<Message>(label: impl Into<String>, on_press: Message) -> Tool<Message> {
    Tool {
        label: label.into(),
        icon: None,
        on_press,
        enabled: true,
        toggled: false,
    }
}

impl<Message: Clone> Tool<Message> {
    /// Sets the tool's icon (rendered from the installed icon font).
    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Enables or disables the tool (a disabled tool shows no press).
    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Marks the tool toggled: it keeps the selection background (a
    /// panel button while its panel is open).
    #[must_use]
    pub fn toggled(mut self, toggled: bool) -> Self {
        self.toggled = toggled;
        self
    }
}

/// A strip of [`Tool`]s: leading tools pinned left, the main tools
/// centred, trailing pinned right — dopus's navigation strip shape,
/// which keeps the centre centred whatever the sidebars weigh.
#[must_use]
pub struct Toolbar<Message> {
    leading: Vec<Tool<Message>>,
    tools: Vec<Tool<Message>>,
    trailing: Vec<Tool<Message>>,
}

impl<Message> Default for Toolbar<Message> {
    fn default() -> Self {
        Self {
            leading: Vec::new(),
            tools: Vec::new(),
            trailing: Vec::new(),
        }
    }
}

impl<Message: Clone> Toolbar<Message> {
    /// An empty [`Toolbar`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a centred tool.
    #[must_use]
    pub fn push(mut self, tool: Tool<Message>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Adds a leading (left-pinned) tool.
    #[must_use]
    pub fn leading(mut self, tool: Tool<Message>) -> Self {
        self.leading.push(tool);
        self
    }

    /// Adds a trailing (right-pinned) tool.
    #[must_use]
    pub fn trailing(mut self, tool: Tool<Message>) -> Self {
        self.trailing.push(tool);
        self
    }

    /// Draws the strip.
    pub fn view<'a>(self, tokens: Tokens) -> Element<'a, Message, Theme, Renderer>
    where
        Message: 'a,
    {
        let m = &tokens.metrics;
        let gap = m.spacing.sm;

        let side = |tools: Vec<Tool<Message>>| -> Element<'a, Message, Theme, Renderer> {
            row(tools.into_iter().map(|tool| tool_view(tool, tokens)))
                .spacing(gap)
                .align_y(iced_core::alignment::Vertical::Center)
                .into()
        };

        let centre = row(self.tools.into_iter().map(|tool| tool_view(tool, tokens)))
            .spacing(gap)
            .align_y(iced_core::alignment::Vertical::Center);

        // Equal fill-width edge groups keep the main tools centred even
        // when only one edge has controls. At narrow sizes the app should
        // reduce its tools, just as it would for any toolbar.
        let tools = row![
            container(side(self.leading)).width(Length::Fill),
            centre,
            container(side(self.trailing))
                .width(Length::Fill)
                .align_x(iced_core::alignment::Horizontal::Right),
        ]
        .spacing(gap)
        .align_y(iced_core::alignment::Vertical::Center);

        container(tools)
            .width(Length::Fill)
            .padding([m.spacing.xs, m.spacing.sm])
            .style(strip)
            .into()
    }
}

/// One toolbar control's button, with its label as the tooltip.
fn tool_view<'a, Message: Clone + 'a>(
    tool: Tool<Message>,
    tokens: Tokens,
) -> Element<'a, Message, Theme, Renderer> {
    let m = &tokens.metrics;
    let Tool {
        label,
        icon,
        on_press,
        enabled,
        toggled,
    } = tool;

    let mut content = row![]
        .spacing(m.spacing.xs)
        .align_y(iced_core::alignment::Vertical::Center);
    if let Some(icon) = icon {
        content = content.push(icon.size(m.text.sm));
    }
    content = content.push(text(label.clone()).size(m.text.sm));

    let mut press = button(content).padding(Padding::from([m.spacing.xs, m.spacing.sm]));
    if enabled {
        press = press.on_press(on_press);
    }

    press = press.style(move |theme, status| {
        let t = theme.tokens().palette;
        let mut style = theme::button::secondary(theme, status);
        if toggled && status != button::Status::Disabled {
            style.background = Some(iced_core::Background::Color(t.selection));
            style.text_color = t.selection_text;
        }
        style
    });

    crate::tips::tip(&tokens, press, label.clone())
}

/// One navigation entry: a label, an optional icon, whether it is the
/// selected one, and the message a click produces.
#[must_use]
pub struct Place<Message> {
    label: String,
    icon: Option<Icon>,
    selected: bool,
    on_press: Message,
}

/// Creates a [`Place`].
pub fn place<Message>(
    label: impl Into<String>,
    selected: bool,
    on_press: Message,
) -> Place<Message> {
    Place {
        label: label.into(),
        icon: None,
        selected,
        on_press,
    }
}

impl<Message: Clone> Place<Message> {
    /// Sets the entry's icon.
    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }
}

/// A scrollable column of [`Place`]s — the places sidebar pattern. The
/// selected entry keeps the selection background from the live theme.
#[must_use]
pub fn places<'a, Message: Clone + 'a>(
    tokens: Tokens,
    entries: Vec<Place<Message>>,
) -> Element<'a, Message, Theme, Renderer> {
    let m = &tokens.metrics;

    let mut list = column![].spacing(m.spacing.xs).width(Length::Fill);
    for entry in entries {
        let Place {
            label,
            icon,
            selected,
            on_press,
        } = entry;

        let mut content = row![]
            .spacing(m.spacing.sm)
            .align_y(iced_core::alignment::Vertical::Center);
        if let Some(icon) = icon {
            content = content.push(icon.size(m.text.sm));
        }
        content = content.push(
            text(label)
                .size(m.text.sm)
                .width(Length::Fill)
                .wrapping(iced_core::text::Wrapping::None)
                .ellipsis(iced_core::text::Ellipsis::End)
                .style(move |theme| {
                    let t = theme.tokens().palette;
                    iced_core::widget::text::Style {
                        color: Some(if selected {
                            t.selection_text
                        } else {
                            t.muted_text
                        }),
                    }
                }),
        );

        let press = button(content)
            .padding(Padding::from([m.spacing.xs, m.spacing.sm]))
            .width(Length::Fill)
            .on_press(on_press)
            .style(move |theme, status| {
                let t = theme.tokens().palette;
                iced_widget::button::Style {
                    background: match (selected, status) {
                        (true, _) => Some(iced_core::Background::Color(t.selection)),
                        (false, button::Status::Hovered | button::Status::Pressed) => {
                            Some(iced_core::Background::Color(t.muted_surface))
                        }
                        (false, _) => None,
                    },
                    text_color: if selected {
                        t.selection_text
                    } else {
                        t.muted_text
                    },
                    border: iced_core::Border {
                        radius: theme.tokens().metrics.radius.sm.into(),
                        ..iced_core::Border::default()
                    },
                    ..iced_widget::button::Style::default()
                }
            });

        list = list.push(press);
    }

    container(Scrollable::new(list))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(strip)
        .into()
}

/// The kind of a status field: how it draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FieldKind {
    /// Normal text.
    #[default]
    Plain,
    /// Muted text (meta the eye should skip).
    Quiet,
    /// The destructive colour (alarms).
    Alarm,
}

/// One status bar field.
#[must_use]
pub struct Field {
    /// The field's text.
    pub text: String,
    /// How it draws.
    pub kind: FieldKind,
}

/// Creates a plain [`Field`].
pub fn field(text: impl Into<String>) -> Field {
    Field {
        text: text.into(),
        kind: FieldKind::default(),
    }
}

impl Field {
    /// Sets the kind.
    #[must_use]
    pub fn kind(mut self, kind: FieldKind) -> Self {
        self.kind = kind;
        self
    }
}

/// The status bar: quiet fields left, the rest right, over the muted
/// strip — ced's status bar shape.
#[must_use]
#[derive(Default)]
pub struct StatusBar {
    left: Vec<Field>,
    right: Vec<Field>,
}

impl StatusBar {
    /// An empty [`StatusBar`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds left-hand (leading) fields.
    #[must_use]
    pub fn left(mut self, fields: Vec<Field>) -> Self {
        self.left.extend(fields);
        self
    }

    /// Adds right-hand (trailing) fields.
    #[must_use]
    pub fn right(mut self, fields: Vec<Field>) -> Self {
        self.right.extend(fields);
        self
    }

    /// Draws the bar.
    pub fn view<'a, Message: 'a>(self, tokens: Tokens) -> Element<'a, Message, Theme, Renderer> {
        let m = &tokens.metrics;
        let gap = m.spacing.md;

        let field_view = |field: Field| -> Element<'a, Message, Theme, Renderer> {
            let kind = field.kind;
            text(field.text)
                .size(m.text.xs)
                .style(move |theme| {
                    let t = theme.tokens().palette;
                    iced_core::widget::text::Style {
                        color: Some(match kind {
                            FieldKind::Plain => t.text,
                            FieldKind::Quiet => t.muted_text,
                            FieldKind::Alarm => t.destructive,
                        }),
                    }
                })
                .into()
        };

        let left = row(self.left.into_iter().map(field_view))
            .spacing(gap)
            .align_y(iced_core::alignment::Vertical::Center);

        let right = row(self.right.into_iter().map(field_view))
            .spacing(gap)
            .align_y(iced_core::alignment::Vertical::Center);

        container(
            row![left, iced_widget::Space::new().width(Length::Fill), right]
                .spacing(gap)
                .align_y(iced_core::alignment::Vertical::Center),
        )
        .width(Length::Fill)
        .padding([m.spacing.xs, m.spacing.sm])
        .style(strip)
        .into()
    }
}

/// Which side of the content a sidebar sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Side {
    /// Left of the content.
    #[default]
    Left,
    /// Right of the content.
    Right,
}

/// The application shell: menu bar, toolbar, split sidebars, content,
/// status bar — in that vertical order, everything optional except the
/// content.
#[must_use]
pub struct Shell<'a, Message: Clone> {
    menu: Option<Vec<Item<Message>>>,
    toolbar: Option<Toolbar<Message>>,
    sidebar_left: Option<(Element<'a, Message, Theme, Renderer>, f32)>,
    sidebar_right: Option<(Element<'a, Message, Theme, Renderer>, f32)>,
    content: Element<'a, Message, Theme, Renderer>,
    status: Option<StatusBar>,
    on_split: Option<std::rc::Rc<dyn Fn(Side, f32) -> Message + 'a>>,
    tokens: Tokens,
    width: Length,
    height: Length,
}

impl<'a, Message: Clone> Shell<'a, Message> {
    /// A shell showing `content`.
    pub fn new(content: impl Into<Element<'a, Message, Theme, Renderer>>) -> Self {
        Self {
            menu: None,
            toolbar: None,
            sidebar_left: None,
            sidebar_right: None,
            content: content.into(),
            status: None,
            on_split: None,
            tokens: Tokens::dark(),
            width: Length::Fill,
            height: Length::Fill,
        }
    }

    /// Sets the menu bar (toolkit's [`menu`](crate::menu)); `None`
    /// draws none.
    #[must_use]
    pub fn menu(mut self, menu: Option<Vec<Item<Message>>>) -> Self {
        self.menu = menu;
        self
    }

    /// Sets the toolbar.
    #[must_use]
    pub fn toolbar(mut self, toolbar: Toolbar<Message>) -> Self {
        self.toolbar = Some(toolbar);
        self
    }

    /// Adds a sidebar `width` px wide on `side`, split from the content
    /// with the grip.
    #[must_use]
    pub fn sidebar(
        mut self,
        side: Side,
        width: f32,
        sidebar: impl Into<Element<'a, Message, Theme, Renderer>>,
    ) -> Self {
        match side {
            Side::Left => self.sidebar_left = Some((sidebar.into(), width)),
            Side::Right => self.sidebar_right = Some((sidebar.into(), width)),
        }
        self
    }

    /// Makes the sidebar grips draggable: the message fires with the new
    /// split position and side (pixels from that side's outer edge).
    #[must_use]
    pub fn on_split(mut self, on_split: impl Fn(Side, f32) -> Message + 'a) -> Self {
        self.on_split = Some(std::rc::Rc::new(on_split));
        self
    }

    /// Sets the layout metrics and tooltip tokens. Rebuild the view with
    /// the application's current tokens when its theme changes. Colours
    /// for bars, tools and places resolve from the live drawing theme.
    #[must_use]
    pub fn tokens(mut self, tokens: Tokens) -> Self {
        self.tokens = tokens;
        self
    }

    /// Sets the status bar.
    #[must_use]
    pub fn status(mut self, status: StatusBar) -> Self {
        self.status = Some(status);
        self
    }

    /// Sets the shell's size strategy.
    #[must_use]
    pub fn size(mut self, width: impl Into<Length>, height: impl Into<Length>) -> Self {
        self.width = width.into();
        self.height = height.into();
        self
    }
}

impl<'a, Message: Clone + 'a> From<Shell<'a, Message>> for Element<'a, Message, Theme, Renderer> {
    fn from(shell: Shell<'a, Message>) -> Self {
        let Shell {
            menu,
            toolbar,
            sidebar_left,
            sidebar_right,
            content,
            status,
            on_split,
            tokens,
            width,
            height,
        } = shell;

        let mut app = column![].width(width).height(height);

        if let Some(items) = menu {
            app = app.push(menu::Menu::bar(items));
        }
        if let Some(toolbar) = toolbar {
            app = app.push(toolbar.view(tokens));
        }

        let mut body = content;
        if let Some((sidebar, px)) = sidebar_left {
            let mut split = Split::new(px, sidebar, body).strategy(split::Strategy::Start);
            if let Some(on_split) = on_split.clone() {
                split = split.on_drag(move |v| on_split(Side::Left, v));
            }
            body = split.into();
        }
        if let Some((sidebar, px)) = sidebar_right {
            let mut split = Split::new(px, body, sidebar).strategy(split::Strategy::End);
            if let Some(on_split) = on_split {
                split = split.on_drag(move |v| on_split(Side::Right, v));
            }
            body = split.into();
        }
        app = app.push(body);

        if let Some(status) = status {
            app = app.push(status.view(tokens));
        }

        app.into()
    }
}
