//! One scene drawn with iced: the ten families as iced widgets.
//!
//! Laid out with iced, not a flexbox engine. Where iced has no flexbox
//! equivalent the view approximates, and says so at the spot: `justify`
//! becomes fill spaces, `fill` is a Fill on the parent's main axis, `grow` a
//! FillPortion, `align: stretch` a Fill on the cross axis; `shrink` and
//! `basis` have no iced counterpart. Minimum and maximum bounds use iced's
//! bounded lengths; equal bounds are fixed. Pixel-identical layout with Quoin
//! is not a goal.
//!
//! The UI is runtime-side state only: what it shows arrives as a
//! [`SceneMessage::Replace`] per accepted revision, and every interaction
//! leaves as a message the host's handler turns into a Bus event.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use decor::{Palette, Srgba};
use design::{
    ButtonCellKey, ButtonSize, ButtonVariant, InteractionState, LinearRgba, ResolvedButtonTable,
};
use iced_core::alignment::{Horizontal, Vertical};
use iced_core::text::{Ellipsis, Wrapping};
use iced_core::{Background, Border, Color, Element, Font, Length, Padding, Theme, font};
use iced_widget::{Column, Row, Space, button, container, scrollable, text, toggler};
use scene::{Node, ResolvedScene};
use serde_json::{Value, json};
use ui::engine::ui::EventFlags;
use ui::engine::{IcedUi, Renderer};

use crate::templates::{PreparedLists, children, rows, text as port_text};

/// Text size where a node names none (Quoin's 13 px).
const TEXT_SIZE: f32 = 13.0;
/// The dialog frame's title bar (the frozen layout puts `root` at y 32).
pub const TITLEBAR: f32 = 32.0;

/// What one surface draws: an accepted revision and its prepared rows.
#[derive(Debug)]
pub struct Content {
    pub tree: ResolvedScene,
    pub lists: PreparedLists,
    pub revision: u64,
    /// The dialog frame, when the scene is a dialog with `chrome:true`.
    pub frame: Option<Frame>,
    /// The scene sits in the dialog seat (framed or not): Escape hides it.
    pub dialog: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub title: String,
}

/// Where an interaction goes: the citizen, the handler per kind, and the
/// payload's identity. A node inside a list row carries the LIST's route and
/// the row item, as Quoin's rows do.
#[derive(Debug)]
pub struct Route {
    pub scene: String,
    pub citizen: String,
    pub node: String,
    pub item: Option<Value>,
    pub click: Option<String>,
    pub change: Option<String>,
    pub submit: Option<String>,
}

impl Route {
    fn new(tree: &ResolvedScene, id: &str, node: &Node) -> Route {
        let port = |name: &str| {
            node.ports
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        Route {
            scene: tree.name.clone(),
            citizen: tree.citizen.clone(),
            node: id.to_owned(),
            item: None,
            click: port("on_click"),
            change: port("on_change"),
            submit: port("on_submit"),
        }
    }

    /// The handler for `kind` and the event body `{scene,node,kind,value?,item?}`.
    pub fn event(&self, kind: &str, value: Option<Value>) -> Option<(&str, &str, Value)> {
        let handler = match kind {
            "click" => self.click.as_deref(),
            "change" => self.change.as_deref(),
            _ => self.submit.as_deref(),
        }?;
        let mut body = json!({"scene": self.scene, "node": self.node, "kind": kind});
        if let Some(value) = value {
            body["value"] = value;
        }
        if let Some(item) = &self.item {
            body["item"] = item.clone();
        }
        Some((&self.citizen, handler, body))
    }
}

#[derive(Clone, Debug)]
pub enum SceneMessage {
    /// A new revision to draw.
    Replace(Arc<Content>),
    /// Restyle the existing widget tree without altering local edits or focus.
    Appearance(Arc<::appearance::settings::Prepared>),
    Click(Arc<Route>),
    Toggle(Arc<Route>, String, bool),
    Input(Arc<Route>, String, String),
    Submit(Arc<Route>, String),
    /// The dialog frame's close button.
    Close,
    EscapeEdge,
    EdgeFocus(bool),
}

/// One scene surface's iced UI.
pub struct SceneUi {
    content: Arc<Content>,
    palette: Palette,
    theme: Theme,
    buttons: Option<Arc<ResolvedButtonTable>>,
    prepared: Option<Arc<::appearance::settings::Prepared>>,
    /// Local text of the fields being edited, by instance key, until the
    /// scene's own `value` port changes.
    edits: BTreeMap<String, String>,
    /// Local toggle states, likewise.
    toggles: BTreeMap<String, bool>,
    images: crate::images::Prepared,
}

fn color(c: Srgba) -> Color {
    Color::from_rgba(c.r, c.g, c.b, c.a)
}

/// Quoin's PANEL token is the design's secondary surface, always in dark
/// mode. Chrome may be light: resolve this context once for all edge pages,
/// retaining the shared source's scheme and overrides. Dialogs use chrome.
#[derive(Clone)]
struct SceneDesign {
    palette: Palette,
    buttons: Arc<ResolvedButtonTable>,
}

fn scene_design(dialog: bool) -> SceneDesign {
    static EDGE: OnceLock<SceneDesign> = OnceLock::new();
    static DIALOG: OnceLock<SceneDesign> = OnceLock::new();
    let design = if dialog { &DIALOG } else { &EDGE };
    design
        .get_or_init(|| {
            compile_scene_design(design::EMBEDDED_DEFAULT_SOURCE, dialog)
                .expect("the design library's embedded scene design compiles")
        })
        .clone()
}

fn compile_scene_design(source: &str, dialog: bool) -> Option<SceneDesign> {
    use design::{DesignCompileResult, DesignContext, Mode, Scheme, SourceIdentity};

    let (document, selection) =
        match design::parse_design_source(SourceIdentity::new("compd:scene"), source) {
            Ok(document) => {
                let selection = document.legacy.clone();
                (document, selection)
            }
            Err(_) => {
                let value = config::parse(source).ok()?;
                let config::Value::Map(fields) = &value else {
                    return None;
                };
                if fields.keys().any(|key| key != "scheme" && key != "mode") {
                    return None;
                }
                let selection = design::parse_legacy_v0_source(source).ok()?;
                if !selection.is_selection_only() {
                    return None;
                }
                let document = design::parse_design_source(
                    SourceIdentity::new("compd:scene:selection"),
                    design::EMBEDDED_DEFAULT_SOURCE,
                )
                .ok()?;
                (document, selection)
            }
        };
    let context = DesignContext {
        scheme: selection
            .scheme
            .as_deref()
            .map(Scheme::from_name)
            .unwrap_or(Some(Scheme::default()))?,
        mode: if dialog {
            selection
                .mode
                .as_deref()
                .map(Mode::from_name)
                .unwrap_or(Some(Mode::default()))?
        } else {
            Mode::Dark
        },
        ..DesignContext::default()
    };
    let DesignCompileResult::Success(success) = design::compile_design(&document, context) else {
        return None;
    };
    let mut palette = Palette::from_design(&success.candidate);
    // Use the whole pair so unstyled text and iced's default controls agree
    // with the page. No scene colour or iced fallback chooses the page fill.
    if !dialog {
        palette.base = palette.secondary;
    }
    Some(SceneDesign {
        palette,
        buttons: Arc::new(success.candidate.tables().button.clone()),
    })
}

fn design_color(c: LinearRgba) -> Color {
    Color::from_linear_rgba(c.red as f32, c.green as f32, c.blue as f32, c.alpha as f32)
}

/// Quoin's scene `tone` selects a canonical CTK button. Its design cell owns
/// both halves of the pair; scene `background`, `hover` and `color` ports do
/// not override a button (those belong to rows/columns and text respectively).
/// Do not pass this pair through iced's generated contrast palette.
fn scene_button_style(
    buttons: &ResolvedButtonTable,
    node: &Node,
    status: button::Status,
) -> button::Style {
    let cell = buttons.cell(scene_button_key(node, status));
    button::Style {
        background: Some(Background::Color(design_color(cell.pair.surface))),
        text_color: design_color(cell.pair.foreground),
        border: Border {
            color: cell.border.map(design_color).unwrap_or(Color::TRANSPARENT),
            width: cell.border_width as f32,
            radius: (cell.radius as f32).into(),
        },
        ..button::Style::default()
    }
}

fn scene_button_key(node: &Node, status: button::Status) -> ButtonCellKey {
    let variant = match port_text(node, "tone") {
        "primary" => ButtonVariant::Primary,
        "danger" => ButtonVariant::Destructive,
        _ => ButtonVariant::default(),
    };
    let interaction = match status {
        button::Status::Active => InteractionState::Resting,
        button::Status::Hovered => InteractionState::Hovered,
        button::Status::Pressed => InteractionState::Pressed,
        button::Status::Disabled => InteractionState::Disabled,
    };
    ButtonCellKey {
        variant,
        size: ButtonSize::default(),
        interaction,
        focus_visible: false,
    }
}

fn read_button_style(cell: &design::ReadButton) -> button::Style {
    let color =
        |[r, g, b, a]: [f64; 4]| Color::from_linear_rgba(r as f32, g as f32, b as f32, a as f32);
    button::Style {
        background: Some(Background::Color(color(cell.pair.surface))),
        text_color: color(cell.pair.foreground),
        border: Border {
            color: cell.border.map(color).unwrap_or(Color::TRANSPARENT),
            width: cell.border_width as f32,
            radius: (cell.radius as f32).into(),
        },
        ..button::Style::default()
    }
}

/// Scene rows keep their authored fill/hover. A child text node resolves its
/// own colour; the row's button wrapper must not seed an iced tone palette.
fn scene_row_style(node: &Node, ink: Color, status: button::Status) -> button::Style {
    let normal = hex(port_text(node, "background"));
    let hover = hex(port_text(node, "hover")).or(normal);
    button::Style {
        background: match status {
            button::Status::Hovered | button::Status::Pressed => hover,
            _ => normal,
        }
        .map(Background::Color),
        text_color: ink,
        border: Border {
            radius: number(node, "radius").unwrap_or(0.0).into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`, as Quoin parses scene colours.
pub fn hex(value: &str) -> Option<Color> {
    let digits = value.strip_prefix('#')?;
    let nibble = |i: usize| {
        u8::from_str_radix(&digits[i..i + 1], 16)
            .ok()
            .map(|v| v * 17)
    };
    let byte = |i: usize| u8::from_str_radix(digits.get(i..i + 2)?, 16).ok();
    if !digits.is_ascii() {
        return None;
    }
    let (r, g, b, a) = match digits.len() {
        3 => (nibble(0)?, nibble(1)?, nibble(2)?, 255),
        4 => (nibble(0)?, nibble(1)?, nibble(2)?, nibble(3)?),
        6 => (byte(0)?, byte(2)?, byte(4)?, 255),
        8 => (byte(0)?, byte(2)?, byte(4)?, byte(6)?),
        _ => return None,
    };
    Some(Color::from_rgba8(r, g, b, a as f32 / 255.0))
}

/// `#rrggbbaa`, the form [`hex`] reads back (sRGB, rounded).
pub fn hex_of(c: Color) -> String {
    let [r, g, b, a] = c.into_rgba8();
    format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
}

fn number(node: &Node, port: &str) -> Option<f32> {
    node.ports
        .get(port)
        .and_then(Value::as_f64)
        .map(|v| v as f32)
}

fn flag(node: &Node, port: &str) -> bool {
    node.ports
        .get(port)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The rows a vertical list shows before it scrolls: Quoin's
/// `min(rows, max_rows (8)).max(1) * (row_height + gap)`.
pub fn list_height(node: &Node) -> f32 {
    let shown = rows(node)
        .len()
        .min(number(node, "max_rows").unwrap_or(8.0) as usize)
        .max(1);
    shown as f32 * (number(node, "row_height").unwrap_or(24.0) + number(node, "gap").unwrap_or(0.0))
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    Vertical,
    Horizontal,
}

/// Where a node's ports are read from: the document, or one list row's
/// instantiated template.
#[derive(Clone, Copy)]
enum Nodes<'a> {
    Tree(&'a ResolvedScene),
    Row(&'a BTreeMap<String, Node>),
}

impl<'a> Nodes<'a> {
    fn get(self, id: &str) -> Option<&'a Node> {
        match self {
            Nodes::Tree(tree) => tree.nodes.get(id),
            Nodes::Row(nodes) => nodes.get(id),
        }
    }
}

fn intrinsic_height(
    nodes: Nodes<'_>,
    id: &str,
    depth: usize,
    styles: [toolkit::typography::TextStyle; 2],
) -> Option<f32> {
    if depth > 64 {
        return None;
    }
    let node = nodes.get(id)?;
    if flag(node, "hidden") {
        return None;
    }
    if let Some(height) = number(node, "height") {
        return Some(height);
    }
    let height = match node.family.as_str() {
        "text" => {
            let style = styles[usize::from(flag(node, "mono"))];
            style
                .line_height
                .unwrap_or(number(node, "size").unwrap_or(style.size) * 1.2)
        }
        "image" => number(node, "h").unwrap_or(16.0),
        "row" => children(node)
            .filter_map(|id| intrinsic_height(nodes, id, depth + 1, styles))
            .fold(0.0_f32, f32::max),
        "column" => {
            let heights: Vec<_> = children(node)
                .filter_map(|id| intrinsic_height(nodes, id, depth + 1, styles))
                .collect();
            heights.iter().sum::<f32>()
                + heights.len().saturating_sub(1) as f32 * number(node, "gap").unwrap_or(0.0)
        }
        _ => 0.0,
    };
    Some(height)
}

/// Focus and acknowledge the actual field, rather than marking a request
/// complete merely because an operation was issued.
pub(crate) struct FocusField {
    target: iced_core::widget::Id,
    pub found: bool,
}

impl FocusField {
    pub(crate) fn new(id: &str) -> Self {
        Self {
            target: field_id(id),
            found: false,
        }
    }
}

impl iced_core::widget::Operation for FocusField {
    fn focusable(
        &mut self,
        id: Option<&iced_core::widget::Id>,
        _bounds: iced_core::Rectangle,
        state: &mut dyn iced_core::widget::operation::Focusable,
    ) {
        if id == Some(&self.target) {
            state.focus();
            self.found = state.is_focused();
        } else {
            state.unfocus();
        }
    }

    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn iced_core::widget::Operation)) {
        operate(self);
    }
}

/// The parent's layout as a child sees it.
#[derive(Clone, Copy)]
struct Parent {
    main: Axis,
    stretch: bool,
}

type El<'a, R = Renderer> = Element<'a, SceneMessage, Theme, R>;

// Build the same widget tree for the compositor and adapter-free layout tests.
trait SceneRenderer:
    iced_core::text::Renderer<Font = Font>
    + iced_core::image::Renderer<Handle = iced_core::image::Handle>
    + iced_core::svg::Renderer
    + 'static
{
}

impl<R> SceneRenderer for R where
    R: iced_core::text::Renderer<Font = Font>
        + iced_core::image::Renderer<Handle = iced_core::image::Handle>
        + iced_core::svg::Renderer
        + 'static
{
}

/// The widget id of the frame's close button.
pub const CLOSE_ID: &str = "chrome:close";

/// The widget id of document node `id`.
pub fn node_id(id: &str) -> iced_core::widget::Id {
    iced_core::widget::Id::from(format!("n:{id}"))
}

/// Separate from the measuring container's id: focus operations visit fields.
pub fn field_id(id: &str) -> iced_core::widget::Id {
    iced_core::widget::Id::from(format!("field:{id}"))
}

/// Optional host metadata: `window: { ..., "autofocus": "search" }` opts an
/// edge into keyboard focus when its reveal finishes, including pointer reveals.
/// This explicit opt-in extends §4.3's usual pointer-does-not-focus rule.
pub fn autofocus(tree: &ResolvedScene) -> Option<&str> {
    let id = tree.window.as_ref()?.get("autofocus")?.as_str()?;
    fn visible(tree: &ResolvedScene, id: &str, target: &str, depth: usize) -> bool {
        if depth > tree.nodes.len() {
            return false;
        }
        let Some(node) = tree.nodes.get(id) else {
            return false;
        };
        !flag(node, "hidden")
            && ((id == target && node.family == "field")
                || children(node).any(|child| visible(tree, child, target, depth + 1)))
    }
    visible(tree, "root", id, 0).then_some(id)
}

/// The widget id of list `list`'s row `item`.
pub fn row_id(list: &str, item: &str) -> iced_core::widget::Id {
    iced_core::widget::Id::from(format!("i:{list}:{item}"))
}

impl SceneUi {
    pub fn new(content: Arc<Content>, palette: Palette) -> SceneUi {
        let dialog = content.dialog;
        Self::with_design(content, palette, || scene_design(dialog))
    }

    fn with_design(
        content: Arc<Content>,
        palette: Palette,
        design: impl FnOnce() -> SceneDesign,
    ) -> SceneUi {
        let design = design();
        let palette = if content.dialog {
            palette
        } else {
            design.palette
        };
        let seed = iced_core::theme::palette::Seed {
            background: color(palette.base.surface),
            text: color(palette.base.foreground),
            primary: color(palette.primary.surface),
            success: color(palette.primary.surface),
            warning: color(palette.destructive.surface),
            danger: color(palette.destructive.surface),
        };
        SceneUi {
            images: crate::images::prepare(&content.tree, &content.lists),
            content,
            palette,
            theme: Theme::custom("design", seed),
            buttons: Some(design.buttons),
            prepared: None,
            edits: BTreeMap::new(),
            toggles: BTreeMap::new(),
        }
    }

    /// The opaque page: prepared settings' `secondary` pair for an edge,
    /// or `base` pair for a dialog. Before preparation, use embedded defaults.
    /// `shell.scene.layout` reports it as `page`.
    pub fn page(&self) -> Color {
        color(self.palette.base.surface)
    }

    pub(crate) fn apply_appearance(&mut self, prepared: Arc<::appearance::settings::Prepared>) {
        (self.palette, self.theme) = crate::appearance::page(&prepared, self.content.dialog);
        self.prepared = Some(prepared);
    }

    pub(crate) fn from_prepared(
        content: Arc<Content>,
        prepared: Arc<::appearance::settings::Prepared>,
    ) -> Self {
        let (palette, theme) = crate::appearance::page(&prepared, content.dialog);
        Self {
            images: crate::images::prepare(&content.tree, &content.lists),
            content,
            palette,
            theme,
            buttons: None,
            prepared: Some(prepared),
            edits: BTreeMap::new(),
            toggles: BTreeMap::new(),
        }
    }

    fn text_style(&self, mono: bool) -> toolkit::typography::TextStyle {
        self.prepared
            .as_ref()
            .and_then(|prepared| prepared.typography().get(if mono { "mono" } else { "ui" }))
            .unwrap_or(toolkit::typography::TextStyle {
                font: if mono {
                    Font::MONOSPACE
                } else {
                    ui::font::BODY
                },
                size: TEXT_SIZE,
                line_height: None,
            })
    }

    /// The matching design foreground for text that declares none.
    fn ink(&self) -> Color {
        color(self.palette.base.foreground)
    }

    fn sizing(node: &Node, parent: Parent) -> (Length, Length) {
        // Quoin's unsized spacer grows even without a `fill` port. Apply this
        // to its measuring wrapper too, or iced shrinks away the inner space.
        let fill =
            flag(node, "fill") || (node.family == "spacer" && number(node, "size").is_none());
        let grow = number(node, "grow")
            .filter(|g| *g > 0.0)
            .map(|g| Length::FillPortion(g.round().max(1.0) as u16));
        let main = grow.unwrap_or(if fill { Length::Fill } else { Length::Shrink });
        let cross = if parent.stretch {
            Length::Fill
        } else {
            Length::Shrink
        };
        let (mut width, mut height) = match parent.main {
            Axis::Horizontal => (main, cross),
            Axis::Vertical => (cross, main),
        };
        if let Some(w) = number(node, "width") {
            width = Length::Fixed(w);
        }
        if let Some(h) = number(node, "height") {
            height = Length::Fixed(h);
            // Compact centred cells (pager/chips) have at least a square
            // hit target. A shrink row otherwise fits only the narrow digit.
            if node.family == "row"
                && port_text(node, "justify") == "center"
                && !node.ports.contains_key("width")
            {
                width = Length::Shrink.min(h);
            }
        }
        for (port, length) in [("width", &mut width), ("height", &mut height)] {
            if port_text(node, port) == "fill" {
                *length = Length::Fill;
            }
            let min = number(node, &format!("min_{port}"));
            if let Some(min) = min.filter(|min| Some(*min) == number(node, &format!("max_{port}")))
            {
                *length = Length::Fixed(min);
            } else {
                if let Some(min) = min {
                    *length = length.min(min);
                }
                if let Some(max) = number(node, &format!("max_{port}")) {
                    *length = length.max(max);
                }
            }
        }
        if (matches!(port_text(node, "width"), "height" | "square") || flag(node, "square"))
            && let Some(h) = number(node, "height")
        {
            width = Length::Fixed(h);
        }
        if node.family == "spacer"
            && let Some(size) = number(node, "size")
        {
            width = Length::Fixed(size);
            height = Length::Fixed(size);
        }
        if node.family == "image" {
            width = Length::Fixed(number(node, "w").unwrap_or(16.0));
            height = Length::Fixed(number(node, "h").unwrap_or(16.0));
        }
        if node.family == "field" && !node.ports.contains_key("width") && !flag(node, "fill") {
            width = Length::Fixed(160.0);
        }
        (width, height)
    }

    fn element<'a, R: SceneRenderer>(
        &'a self,
        nodes: Nodes<'a>,
        id: &str,
        parent: Parent,
        row: Option<&Arc<Route>>,
        key: &str,
    ) -> Option<El<'a, R>> {
        let node = nodes.get(id)?;
        let image_key = crate::images::key(id, row.map(Arc::as_ref));
        let recovered = self.content.tree.name == "panel"
            && row.is_none()
            && crate::images::panel_fallback(id).is_some()
            && self.images.contains_key(&image_key);
        let recovered_dot = self.content.tree.name == "panel"
            && row.is_none()
            && crate::images::fallback_image(id)
                .is_some_and(|image| self.images.contains_key(&crate::images::key(image, None)));
        if (flag(node, "hidden") && !recovered) || recovered_dot {
            return None;
        }
        let tree = &self.content.tree;
        let route = || {
            row.cloned()
                .unwrap_or_else(|| Arc::new(Route::new(tree, id, node)))
        };
        let key = format!("{key}/{id}");
        let (width, height) = Self::sizing(node, parent);
        let element: El<'a, R> = match node.family.as_str() {
            "column" | "row" => self.container(nodes, id, node, parent, row, &key, route())?,
            "text" => {
                let style = self.text_style(flag(node, "mono"));
                let size = number(node, "size").unwrap_or(style.size);
                // Body text is Light (ui::font::BODY); mono keeps its normal weight.
                let mut font = style.font;
                if flag(node, "bold") {
                    font.weight = font::Weight::Bold;
                }
                let colour = hex(port_text(node, "color")).unwrap_or(self.ink());
                let align = match port_text(node, "align") {
                    "center" => iced_core::text::Alignment::Center,
                    "right" => iced_core::text::Alignment::Right,
                    _ => iced_core::text::Alignment::Left,
                };
                let mut label = text(port_text(node, "text").to_owned())
                    .size(size)
                    .line_height(
                        style
                            .line_height
                            .map_or(iced_core::text::LineHeight::Relative(1.2), |height| {
                                iced_core::text::LineHeight::Absolute(height.into())
                            }),
                    )
                    .font(font)
                    .color(colour)
                    .wrapping(Wrapping::None)
                    .align_x(align)
                    .width(width);
                if flag(node, "elide") {
                    label = label.ellipsis(Ellipsis::Middle);
                }
                label.into()
            }
            "field" => {
                let route = route();
                let value = self
                    .edits
                    .get(&key)
                    .map(String::as_str)
                    .unwrap_or(port_text(node, "value"));
                let (input_route, input_key) = (Arc::clone(&route), key.clone());
                self.text_style(false)
                    .input(port_text(node, "placeholder"), value)
                    .id(field_id(id))
                    .secure(flag(node, "password"))
                    .padding(Padding {
                        top: 4.0,
                        right: 7.0,
                        bottom: 4.0,
                        left: 7.0,
                    })
                    .width(width)
                    .on_input(move |value| {
                        SceneMessage::Input(Arc::clone(&input_route), input_key.clone(), value)
                    })
                    .on_submit(SceneMessage::Submit(route, value.to_owned()))
                    .into()
            }
            "button" => {
                let key = scene_button_key(node, button::Status::Active);
                let label = self
                    .prepared
                    .as_ref()
                    .map_or(self.text_style(false), |prepared| {
                        prepared.button_text(key, design::ButtonPart::Label)
                    });
                button(label.text(port_text(node, "label").to_owned()))
                    .padding(Padding {
                        top: 4.0,
                        right: 10.0,
                        bottom: 4.0,
                        left: 10.0,
                    })
                    .width(width)
                    .style(move |_theme: &Theme, status| {
                        self.prepared.as_ref().map_or_else(
                            || {
                                scene_button_style(
                                    self.buttons.as_ref().expect("legacy scene design"),
                                    node,
                                    status,
                                )
                            },
                            |prepared| {
                                read_button_style(prepared.button(scene_button_key(node, status)))
                            },
                        )
                    })
                    .on_press(SceneMessage::Click(route()))
                    .into()
            }
            "toggle" => {
                let route = route();
                let value = self
                    .toggles
                    .get(&key)
                    .copied()
                    .unwrap_or(flag(node, "value"));
                let toggle_key = key.clone();
                toggler(value)
                    .label(port_text(node, "label").to_owned())
                    .text_size(self.text_style(false).size)
                    .font(self.text_style(false).font)
                    .on_toggle(move |on| {
                        SceneMessage::Toggle(Arc::clone(&route), toggle_key.clone(), on)
                    })
                    .into()
            }
            "list" => self.list(id, node, parent, &key, width, height)?,
            "image" => {
                let (w, h) = (
                    number(node, "w").unwrap_or(16.0),
                    number(node, "h").unwrap_or(16.0),
                );
                if let Some(crate::images::Asset::Svg(handle, symbolic)) =
                    self.images.get(&image_key)
                {
                    let tint =
                        symbolic.then_some(hex(port_text(node, "color")).unwrap_or(self.ink()));
                    iced_widget::Svg::new(handle.clone())
                        .width(w)
                        .height(h)
                        .style(move |_theme: &Theme, _status| iced_widget::svg::Style {
                            color: tint,
                        })
                        .into()
                } else if let Some(crate::images::Asset::Raster(handle)) =
                    self.images.get(&image_key)
                {
                    iced_widget::Image::<iced_core::image::Handle>::new(handle.clone())
                        .width(w)
                        .height(h)
                        .into()
                } else {
                    // Always visible, using the design foreground, even when
                    // no icon theme or application icon is installed.
                    text("▧").size(w.min(h)).color(self.ink()).into()
                }
            }
            "spacer" => match number(node, "size") {
                Some(size) => Space::new().width(size).height(size).into(),
                None => match parent.main {
                    Axis::Horizontal => Space::new().width(Length::Fill).into(),
                    Axis::Vertical => Space::new().height(Length::Fill).into(),
                },
            },
            // The window envelope draws nothing (Quoin: height 0).
            _ => return None,
        };
        // Families that size themselves are returned as built; the rest get
        // their bounded box here.
        let boxed = container(element).width(width).height(height);
        // Document nodes carry their id for the layout read-back; nodes inside
        // a list row are measured as the row (`instances`), not one by one.
        Some(match row {
            None => boxed.id(node_id(id)).into(),
            Some(_) => boxed.into(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn container<'a, R: SceneRenderer>(
        &'a self,
        nodes: Nodes<'a>,
        id: &str,
        node: &'a Node,
        parent: Parent,
        row: Option<&Arc<Route>>,
        key: &str,
        route: Arc<Route>,
    ) -> Option<El<'a, R>> {
        let _ = id;
        let main = if node.family == "row" {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        let align = port_text(node, "align");
        let (width, height) = Self::sizing(node, parent);
        // A bounded shrink flex compresses fill spacers to zero before its
        // minimum is applied. Centre its intrinsic group in a bounded wrapper
        // instead, so the unused part of a compact hit target is shared.
        let centered_shrink = port_text(node, "justify") == "center"
            && matches!(
                if matches!(main, Axis::Horizontal) {
                    width
                } else {
                    height
                },
                Length::Bounded {
                    sizing: iced_core::length::Sizing::Shrink,
                    ..
                }
            );
        let inner = Parent {
            main,
            stretch: align == "stretch",
        };
        let mut kids: Vec<El<'a, R>> = Vec::new();
        let mut footer_start = None;
        for child in children(node) {
            if let Some(element) = self.element(nodes, child, inner, row, key) {
                kids.push(element);
                if nodes
                    .get(child)
                    .is_some_and(|node| node.family == "spacer" && number(node, "size").is_none())
                {
                    footer_start = Some(kids.len());
                }
            }
        }
        // justify: iced has no justify-content; fill spaces stand in.
        let space = |portion| -> El<'a, R> {
            match main {
                Axis::Horizontal => Space::new().width(Length::FillPortion(portion)).into(),
                Axis::Vertical => Space::new().height(Length::FillPortion(portion)).into(),
            }
        };
        match port_text(node, "justify") {
            "center" if !centered_shrink => {
                kids.insert(0, space(1));
                kids.push(space(1));
            }
            "end" => kids.insert(0, space(1)),
            "between" | "around" | "evenly" | "space_between" | "space_around" | "space_evenly"
                if kids.len() > 1 =>
            {
                let edges = !matches!(port_text(node, "justify"), "between" | "space_between");
                let between = if matches!(port_text(node, "justify"), "around" | "space_around") {
                    2
                } else {
                    1
                };
                let mut spaced = Vec::with_capacity(kids.len() * 2 + 1);
                if edges {
                    spaced.push(space(1));
                }
                let count = kids.len();
                for (i, kid) in kids.into_iter().enumerate() {
                    spaced.push(kid);
                    if i + 1 < count {
                        spaced.push(space(between));
                    }
                }
                if edges {
                    spaced.push(space(1));
                }
                kids = spaced;
            }
            _ => {}
        }
        let all = number(node, "padding").unwrap_or(0.0);
        let padding = Padding {
            top: number(node, "padding_top").unwrap_or(all),
            right: number(node, "padding_right").unwrap_or(all),
            bottom: number(node, "padding_bottom").unwrap_or(all),
            left: number(node, "padding_left").unwrap_or(all),
        };
        // iced compresses children into the padded fixed height. Quoin keeps
        // their intrinsic text/image height; preserve that before padding.
        let padding = if let Some(height) = number(node, "height") {
            let content_height = children(node)
                .filter_map(|id| {
                    intrinsic_height(
                        nodes,
                        id,
                        0,
                        [self.text_style(false), self.text_style(true)],
                    )
                })
                .fold(0.0_f32, f32::max);
            let available = (height - content_height).max(0.0);
            let vertical = padding.top + padding.bottom;
            let ratio = if vertical > 0.0 {
                (available / vertical).min(1.0)
            } else {
                1.0
            };
            Padding {
                top: padding.top * ratio,
                bottom: padding.bottom * ratio,
                ..padding
            }
        } else {
            padding
        };
        let gap = number(node, "gap").unwrap_or(0.0);
        let inner_len = |l: Length| {
            if matches!(
                l,
                Length::Shrink
                    | Length::Bounded {
                        sizing: iced_core::length::Sizing::Shrink,
                        ..
                    }
            ) {
                Length::Shrink
            } else {
                Length::Fill
            }
        };
        let body: El<'a, R> = match main {
            Axis::Vertical => {
                let spacing = number(node, "row_gap").unwrap_or(gap);
                let alignment = match align {
                    "center" => Horizontal::Center,
                    "end" => Horizontal::Right,
                    _ => Horizontal::Left,
                };
                // An unsized spacer separates a flexible body from its
                // footer (calendar: cal_fill -> cal_open). Reserve the footer
                // in the outer flex pass before laying out the growing body;
                // a nested fill must never consume the footer's allocation.
                if height.is_fill()
                    && matches!(port_text(node, "justify"), "" | "start")
                    && let Some(split) = footer_start.filter(|split| *split < kids.len())
                {
                    let footer = kids.split_off(split);
                    let body = Column::from_vec(kids)
                        .spacing(spacing)
                        .align_x(alignment)
                        .width(Length::Fill)
                        .height(Length::Fill);
                    kids = vec![body.into()];
                    kids.extend(footer);
                }
                Column::from_vec(kids)
                    .spacing(spacing)
                    .padding(padding)
                    .width(inner_len(width))
                    .height(inner_len(height))
                    .align_x(alignment)
                    .into()
            }
            Axis::Horizontal => Row::from_vec(kids)
                .spacing(number(node, "column_gap").unwrap_or(gap))
                .padding(padding)
                .width(inner_len(width))
                .height(inner_len(height))
                .align_y(match align {
                    "center" => Vertical::Center,
                    "end" => Vertical::Bottom,
                    _ => Vertical::Top,
                })
                .into(),
        };
        let normal = hex(port_text(node, "background"));
        let hover = hex(port_text(node, "hover")).or(normal);
        let radius = number(node, "radius").unwrap_or(0.0);
        let clickable = node.ports.contains_key("on_click") || row.is_some();
        if clickable || hover != normal {
            // A press on a row reaches its handler, and a hover colour needs
            // the button's status; an inner button still takes its own press.
            let text_colour = self.ink();
            let pressable = if centered_shrink {
                toolkit::CenteredButton::new(body)
                    .width(width)
                    .height(height)
                    .align_x(if matches!(main, Axis::Horizontal) {
                        Horizontal::Center
                    } else {
                        Horizontal::Left
                    })
                    .align_y(if matches!(main, Axis::Vertical) {
                        Vertical::Center
                    } else {
                        Vertical::Top
                    })
                    .build()
            } else {
                button(body).padding(0.0).width(width).height(height)
            };
            return Some(
                pressable
                    .style(move |_theme: &Theme, status| scene_row_style(node, text_colour, status))
                    .on_press(SceneMessage::Click(route))
                    .into(),
            );
        }
        let body = if centered_shrink {
            let bounded = toolkit::centered(body).width(width).height(height);
            match main {
                Axis::Horizontal => bounded.align_y(Vertical::Top).into(),
                Axis::Vertical => bounded.align_x(Horizontal::Left).into(),
            }
        } else {
            body
        };
        Some(
            container(body)
                .width(width)
                .height(height)
                .style(move |_theme: &Theme| iced_widget::container::Style {
                    background: normal.map(Background::Color),
                    border: Border {
                        radius: radius.into(),
                        ..Border::default()
                    },
                    ..iced_widget::container::Style::default()
                })
                .into(),
        )
    }

    fn list<'a, R: SceneRenderer>(
        &'a self,
        id: &str,
        node: &'a Node,
        _parent: Parent,
        key: &str,
        width: Length,
        height: Length,
    ) -> Option<El<'a, R>> {
        let items = rows(node);
        if items.is_empty() && flag(node, "hidden_if_empty") {
            return None;
        }
        let data = self.content.lists.get(id)?;
        let horizontal = port_text(node, "flow") == "horizontal";
        let tree = &self.content.tree;
        let template = port_text(node, "row");
        let row_height = number(node, "row_height").unwrap_or(24.0);
        let gap = number(node, "gap").unwrap_or(0.0);
        let mut drawn: Vec<El<'a, R>> = Vec::with_capacity(items.len());
        for item in items {
            let item_id = item["id"].as_str().unwrap_or_default();
            let Some(nodes) = data.instances.get(item_id) else {
                continue;
            };
            let mut route = Route::new(tree, id, node);
            route.item = Some(item.clone());
            let route = Arc::new(route);
            let inner = Parent {
                main: if horizontal {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                },
                stretch: !horizontal,
            };
            let row_key = format!("{key}#{item_id}");
            let Some(content) =
                self.element(Nodes::Row(nodes), template, inner, Some(&route), &row_key)
            else {
                continue;
            };
            // The whole row is the list's click target (Quoin's ClickRow on
            // the row content); a control inside the row takes its own press.
            let text_colour = self.ink();
            let pressable = button(content)
                .padding(0.0)
                .width(if horizontal {
                    Length::Shrink
                } else {
                    Length::Fill
                })
                .height(if horizontal {
                    Length::Shrink
                } else {
                    Length::Fill
                })
                .style(move |_theme: &Theme, _status| iced_widget::button::Style {
                    background: None,
                    text_color: text_colour,
                    ..iced_widget::button::Style::default()
                })
                .on_press(SceneMessage::Click(Arc::clone(&route)));
            let mut cell = container(pressable).id(row_id(id, item_id));
            if !horizontal {
                cell = cell.width(Length::Fill).height(row_height);
            }
            drawn.push(cell.into());
        }
        if horizontal {
            let align = match port_text(node, "align") {
                "center" => Vertical::Center,
                "end" => Vertical::Bottom,
                _ => Vertical::Top,
            };
            return Some(Row::from_vec(drawn).spacing(gap).align_y(align).into());
        }
        Some(
            scrollable(Column::from_vec(drawn).spacing(gap).width(Length::Fill))
                .width(width)
                .height(if height == Length::Shrink {
                    Length::Fixed(list_height(node))
                } else {
                    height
                })
                .into(),
        )
    }

    fn frame<'a, R: SceneRenderer>(&'a self, frame: &'a Frame, body: El<'a, R>) -> El<'a, R> {
        let bar = self.palette.secondary;
        let title = self
            .prepared
            .as_ref()
            .and_then(|prepared| prepared.typography().get("ui_display"))
            .unwrap_or(self.text_style(false))
            .text(frame.title.clone())
            .color(color(bar.foreground));
        let close = button(
            text("×")
                .size(16.0)
                .align_x(iced_core::text::Alignment::Center),
        )
        .width(28.0)
        .height(24.0)
        .padding(0.0)
        .style(iced_widget::button::text)
        .on_press(SceneMessage::Close);
        let close = container(close).id(iced_core::widget::Id::new(CLOSE_ID));
        let titlebar = container(
            Row::new()
                .push(title)
                .push(Space::new().width(Length::Fill))
                .push(close)
                .align_y(Vertical::Center)
                .padding(Padding {
                    top: 4.0,
                    right: 11.0,
                    bottom: 4.0,
                    left: 12.0,
                }),
        )
        .width(Length::Fill)
        .height(TITLEBAR)
        .style(move |_theme: &Theme| iced_widget::container::Style {
            background: Some(Background::Color(color(bar.surface))),
            ..iced_widget::container::Style::default()
        });
        Column::new().push(titlebar).push(body).into()
    }

    /// Which instance keys' local state a new revision overrides: a field
    /// or toggle whose authored `value` changed.
    /// The revision this UI last applied (`applied_revision`).
    pub fn revision(&self) -> u64 {
        self.content.revision
    }

    fn forget_overridden(&mut self, next: &Content) {
        let previous = &self.content.tree;
        let changed = |id: &str| {
            next.tree.nodes.get(id).and_then(|n| n.ports.get("value"))
                != previous.nodes.get(id).and_then(|n| n.ports.get("value"))
        };
        let node_of = |key: &str| key.rsplit('/').next().unwrap_or_default().to_owned();
        // Row instances are keyed under their row; a revision that changes a
        // list's rows drops their local state with them.
        let row_changed = |key: &str| key.contains('#');
        self.edits
            .retain(|key, _| !row_changed(key) && !changed(&node_of(key)));
        self.toggles
            .retain(|key, _| !row_changed(key) && !changed(&node_of(key)));
    }

    fn build_view<R: SceneRenderer>(&self) -> El<'_, R> {
        let tree = &self.content.tree;
        let root = tree
            .nodes
            .get("root")
            .map(|_| "root".to_owned())
            .or_else(|| {
                tree.nodes
                    .keys()
                    .find(|id| !tree.templates.contains(*id))
                    .cloned()
            });
        let top = Parent {
            main: Axis::Vertical,
            stretch: true,
        };
        let body: El<'_, R> = root
            .and_then(|root| self.element(Nodes::Tree(tree), &root, top, None, ""))
            .unwrap_or_else(|| Space::new().into());
        let body: El<'_, R> = container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let page = match &self.content.frame {
            Some(frame) => self.frame(frame, body),
            None => body,
        };
        let (fill, ink) = (self.page(), self.ink());
        let border = self.palette.border;
        let framed = self.content.frame.is_some();
        container(page)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_theme: &Theme| iced_widget::container::Style {
                text_color: Some(ink),
                background: Some(Background::Color(fill)),
                border: Border {
                    color: color(border),
                    width: if framed { 1.0 } else { 0.0 },
                    ..Border::default()
                },
                ..iced_widget::container::Style::default()
            })
            .into()
    }
}

impl IcedUi for SceneUi {
    type Message = SceneMessage;

    fn view(&self) -> El<'_> {
        self.build_view()
    }

    fn update(&mut self, message: SceneMessage) {
        match message {
            SceneMessage::Replace(content) => {
                self.forget_overridden(&content);
                self.images = crate::images::prepare(&content.tree, &content.lists);
                if let Some(prepared) = &self.prepared
                    && self.content.dialog != content.dialog
                {
                    (self.palette, self.theme) = crate::appearance::page(prepared, content.dialog);
                }
                self.content = content;
            }
            SceneMessage::Appearance(prepared) => self.apply_appearance(prepared),
            SceneMessage::Input(_, key, value) => {
                self.edits.insert(key, value);
            }
            SceneMessage::Toggle(_, key, on) => {
                self.toggles.insert(key, on);
            }
            SceneMessage::Click(_)
            | SceneMessage::Submit(..)
            | SceneMessage::Close
            | SceneMessage::EscapeEdge
            | SceneMessage::EdgeFocus(_) => {}
        }
    }

    fn theme(&self) -> Theme {
        self.theme.clone()
    }

    /// A dialog listens for Escape (Quoin's `chrome::dialog`: Escape hides
    /// the dialog while its window holds the keyboard; keys reach this UI
    /// only while it holds the iced keyboard focus).
    fn subscribe(&self) -> EventFlags {
        if self.content.dialog {
            EventFlags::KEYBOARD
        } else {
            EventFlags::KEYBOARD | EventFlags::WINDOW
        }
    }

    fn event_process(&self, event: &iced_core::Event) -> Vec<SceneMessage> {
        match event {
            iced_core::Event::Keyboard(iced_core::keyboard::Event::KeyPressed {
                key: iced_core::keyboard::Key::Named(iced_core::keyboard::key::Named::Escape),
                ..
            }) if self.content.dialog => vec![SceneMessage::Close],
            iced_core::Event::Keyboard(iced_core::keyboard::Event::KeyPressed {
                key: iced_core::keyboard::Key::Named(iced_core::keyboard::key::Named::Escape),
                ..
            }) => vec![SceneMessage::EscapeEdge],
            iced_core::Event::Window(iced_core::window::Event::Focused) => {
                vec![SceneMessage::EdgeFocus(true)]
            }
            iced_core::Event::Window(iced_core::window::Event::Unfocused) => {
                vec![SceneMessage::EdgeFocus(false)]
            }
            _ => Vec::new(),
        }
    }
}

/// The Bus event a message asks for, if its node has a handler for it:
/// `(citizen, handler, body)`.
pub fn event_of(message: &SceneMessage) -> Option<(&str, &str, Value)> {
    match message {
        SceneMessage::Click(route) => route.event("click", None),
        SceneMessage::Toggle(route, _, on) => route.event("change", Some(json!(on))),
        SceneMessage::Input(route, _, value) => route.event("change", Some(json!(value))),
        SceneMessage::Submit(route, value) => route.event("submit", Some(json!(value))),
        SceneMessage::Replace(_)
        | SceneMessage::Appearance(_)
        | SceneMessage::Close
        | SceneMessage::EscapeEdge
        | SceneMessage::EdgeFocus(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_keeps_content_local_edits_and_toggles_and_updates_shared_defaults() {
        let mut ui = test_ui(
            "---\nscene: 1\nname: settings\ncitizen: test\n---\n```mix\nroot: {widget: \"column\", children: [\"entry\", \"enabled\", \"save\"]}\nentry: {widget: \"field\", value: \"authored\"}\nenabled: {widget: \"toggle\", label: \"Enabled\", value: false}\nsave: {widget: \"button\", label: \"Save\", tone: \"primary\"}\n```\n",
        );
        let content = Arc::clone(&ui.content);
        let route = Arc::new(Route::new(
            &content.tree,
            "entry",
            &content.tree.nodes["entry"],
        ));
        ui.update(SceneMessage::Input(
            Arc::clone(&route),
            "/root/entry".into(),
            "unsaved edit".into(),
        ));
        ui.update(SceneMessage::Toggle(route, "/root/enabled".into(), true));
        ui.apply_appearance(crate::appearance::fixture("light", 1.0));
        let old_page = ui.page();
        let old_type = ui.text_style(false);
        let prepared = crate::appearance::fixture("dark", 1.5);
        ui.update(SceneMessage::Appearance(Arc::clone(&prepared)));
        assert!(Arc::ptr_eq(&content, &ui.content));
        assert_eq!(ui.edits["/root/entry"], "unsaved edit");
        assert!(ui.toggles["/root/enabled"]);
        assert_ne!(ui.page(), old_page);
        assert_eq!(ui.text_style(false).size, old_type.size * 1.5);
        let key = scene_button_key(&content.tree.nodes["save"], button::Status::Hovered);
        assert_eq!(
            read_button_style(prepared.button(key)).text_color,
            Color::from_linear_rgba(
                prepared.button(key).pair.foreground[0] as f32,
                prepared.button(key).pair.foreground[1] as f32,
                prepared.button(key).pair.foreground[2] as f32,
                prepared.button(key).pair.foreground[3] as f32
            )
        );
        let newly_created = SceneUi::from_prepared(Arc::clone(&content), prepared);
        assert_eq!(newly_created.page(), ui.page());
        assert_eq!(newly_created.text_style(false), ui.text_style(false));
        let replacement = Arc::new(Content {
            tree: content.tree.clone(),
            lists: content.lists.clone(),
            revision: 2,
            frame: None,
            dialog: true,
        });
        ui.update(SceneMessage::Replace(replacement));
        assert_eq!(
            ui.palette.base,
            decor::Palette::from_dictionary(ui.prepared.as_ref().unwrap().dictionary()).base
        );
        assert_eq!(ui.edits["/root/entry"], "unsaved edit");
        assert!(ui.toggles["/root/enabled"]);
    }

    #[test]
    fn read_button_colours_match_the_compiler_mapper_in_linear_space() {
        let mut ui = test_ui(
            "---\nscene: 1\nname: controls\ncitizen: test\n---\n```mix\nroot: {widget: \"column\", children: [\"default\", \"primary\", \"danger\"]}\ndefault: {widget: \"button\", label: \"Default\"}\nprimary: {widget: \"button\", label: \"Primary\", tone: \"primary\"}\ndanger: {widget: \"button\", label: \"Danger\", tone: \"danger\"}\n```\n",
        );
        let legacy = compile_scene_design(design::EMBEDDED_DEFAULT_SOURCE, true).unwrap();
        ui.apply_appearance(crate::appearance::fixture("light", 1.0));
        for id in ["default", "primary", "danger"] {
            for status in [
                button::Status::Active,
                button::Status::Hovered,
                button::Status::Pressed,
                button::Status::Disabled,
            ] {
                let node = &ui.content.tree.nodes[id];
                let old = scene_button_style(&legacy.buttons, node, status);
                let read = read_button_style(
                    ui.prepared
                        .as_ref()
                        .unwrap()
                        .button(scene_button_key(node, status)),
                );
                assert_eq!(read.background, old.background);
                assert_eq!(read.text_color, old.text_color);
                assert_eq!(read.border, old.border);
            }
        }
    }

    use crate::test_renderer::LayoutRenderer;

    fn test_ui(source: &str) -> SceneUi {
        let tree = scene::resolve(&scene::parse(source).unwrap()).unwrap();
        let lists = crate::templates::validate_templates(&tree).unwrap();
        let palette = decor::ChromeTheme::from_source(decor::ChromeStyle::Mac, None).palette;
        SceneUi::with_design(
            Arc::new(Content {
                tree,
                lists,
                revision: 1,
                frame: None,
                dialog: false,
            }),
            palette,
            || compile_scene_design(design::EMBEDDED_DEFAULT_SOURCE, false).unwrap(),
        )
    }

    #[test]
    fn panel_has_square_pager_two_clock_lines_and_full_task_hit_targets() {
        let renderer = LayoutRenderer::new();
        let ui = test_ui(include_str!("../tests/fixtures/panel-render.scene.mix"));
        let mut element = ui.build_view::<LayoutRenderer>();
        // Match UserInterface::build: diff populates the child states.
        let mut state = iced_core::widget::Tree::empty();
        state.diff(element.as_widget_mut());
        let layout = element.as_widget_mut().layout(
            &mut state,
            &renderer,
            &iced_core::layout::Limits::new(
                iced_core::Size::ZERO,
                iced_core::Size::new(1536.0, 52.0),
            ),
        );
        let mut measure = crate::layout::Measure::default();
        element.as_widget_mut().operate(
            &mut state,
            iced_core::Layout::new(&layout),
            &renderer,
            &mut measure,
        );
        for id in ["1", "2", "3", "4"] {
            let cell = measure.found[&row_id("pager", id)];
            assert!(
                (cell.width - cell.height).abs() < 0.1,
                "pager {id}: {cell:?}"
            );
            assert!((cell.height - 30.0).abs() < 0.1);
        }
        let time = measure.found[&node_id("clock_time")];
        let date = measure.found[&node_id("clock_date")];
        assert!(
            time.height >= 22.0 && date.height >= 13.0,
            "clock: {time:?}, {date:?}"
        );
        assert!(date.y >= time.y + time.height - 0.1 && date.y + date.height <= 48.0);
        let task = measure.found[&row_id("tasks", "task")];
        assert!(task.width >= 218.0 && task.height >= 40.0, "task: {task:?}");
        let peek = measure.found[&node_id("peek_i")];
        assert_eq!((peek.width, peek.height), (20.0, 20.0));
    }

    #[test]
    fn panel_pager_digits_are_drawn_at_the_centre_of_their_hit_targets() {
        let mut renderer = LayoutRenderer::new();
        let ui = test_ui(include_str!("../tests/fixtures/panel-render.scene.mix"));
        let mut element = ui.build_view::<LayoutRenderer>();
        let mut state = iced_core::widget::Tree::empty();
        state.diff(element.as_widget_mut());
        let viewport = iced_core::Rectangle::with_size(iced_core::Size::new(1536.0, 52.0));
        let layout = element.as_widget_mut().layout(
            &mut state,
            &renderer,
            &iced_core::layout::Limits::new(iced_core::Size::ZERO, viewport.size()),
        );
        let mut measure = crate::layout::Measure::default();
        element.as_widget_mut().operate(
            &mut state,
            iced_core::Layout::new(&layout),
            &renderer,
            &mut measure,
        );
        element.as_widget().draw(
            &state,
            &mut renderer,
            &ui.theme,
            &iced_core::renderer::Style::default(),
            iced_core::Layout::new(&layout),
            iced_core::mouse::Cursor::Unavailable,
            &viewport,
        );
        // The fixture draws its four pager labels before the task and clock.
        // Check shaped text positions, not merely the already-square buttons.
        assert!(renderer.paragraphs.len() >= 4);
        for (id, digit) in ["1", "2", "3", "4"].into_iter().zip(&renderer.paragraphs) {
            let cell = measure.found[&row_id("pager", id)];
            let offset = digit.center().x - cell.center().x;
            assert!(
                offset.abs() < 0.1,
                "pager {id} text offset {offset}: digit={digit:?}, cell={cell:?}"
            );
            assert!((digit.center().y - cell.center().y).abs() < 0.1);
            assert!(digit.x >= cell.x && digit.x + digit.width <= cell.x + cell.width);
            assert_eq!((cell.width, cell.height), (30.0, 30.0));
        }
    }

    #[test]
    fn bounded_centred_groups_keep_gaps_and_fit_constrained_rows_and_columns() {
        let source = "---\nscene: 1\nname: centred\ncitizen: test\nwindow: {\"kind\":\"edge\",\"edge\":\"bottom\",\"h\":220}\n---\n```mix\nroot: {widget: \"column\", fill: true, children: [\"r\", \"c\"]}\nr: {widget: \"row\", height: 30, min_width: 60, max_width: 80, padding: 4, gap: 6, justify: \"center\", align: \"center\", children: [\"a\", \"b\"]}\nc: {widget: \"column\", min_width: 60, max_width: 60, min_height: 60, max_height: 80, padding: 4, gap: 6, justify: \"center\", align: \"center\", children: [\"cc\", \"dd\"]}\na: {widget: \"text\", text: \"1\", size: 10}\nb: {widget: \"text\", text: \"2\", size: 10}\ncc: {widget: \"text\", text: \"1\", size: 10}\ndd: {widget: \"text\", text: \"2\", size: 10}\n```\n";
        for width in [120.0, 45.0] {
            let mut renderer = LayoutRenderer::new();
            let ui = test_ui(source);
            let mut element = ui.build_view::<LayoutRenderer>();
            let mut state = iced_core::widget::Tree::empty();
            state.diff(element.as_widget_mut());
            let viewport = iced_core::Rectangle::with_size(iced_core::Size::new(width, 220.0));
            let layout = element.as_widget_mut().layout(
                &mut state,
                &renderer,
                &iced_core::layout::Limits::new(iced_core::Size::ZERO, viewport.size()),
            );
            let mut measure = crate::layout::Measure::default();
            element.as_widget_mut().operate(
                &mut state,
                iced_core::Layout::new(&layout),
                &renderer,
                &mut measure,
            );
            element.as_widget().draw(
                &state,
                &mut renderer,
                &ui.theme,
                &iced_core::renderer::Style::default(),
                iced_core::Layout::new(&layout),
                iced_core::mouse::Cursor::Unavailable,
                &viewport,
            );
            assert_eq!(renderer.paragraphs.len(), 4);
            let row = measure.found[&node_id("r")];
            let column = measure.found[&node_id("c")];
            let [a, b, c, d] = renderer.paragraphs[..] else {
                unreachable!()
            };
            assert!(
                ((a.x + b.x + b.width) / 2.0 - row.center().x).abs() < 0.1,
                "row={row:?}, a={a:?}, b={b:?}"
            );
            assert!(
                ((c.y + d.y + d.height) / 2.0 - column.center().y).abs() < 0.1,
                "column={column:?}, c={c:?}, d={d:?}"
            );
            assert!((b.x - a.x - a.width - 6.0).abs() < 0.1);
            assert!((d.y - c.y - c.height - 6.0).abs() < 0.1);
            assert!((row.width - width.min(60.0)).abs() < 0.1);
            assert!((column.height - 60.0).abs() < 0.1);
            assert!(a.x >= row.x && b.x + b.width <= row.x + row.width);
            assert!(c.y >= column.y && d.y + d.height <= column.y + column.height);
        }
    }

    #[test]
    fn autofocus_after_final_layout_targets_the_field_and_typing_emits_filter() {
        let renderer = LayoutRenderer::new();
        let mut ui = test_ui(
            "---\nscene: 1\nname: launcher\ncitizen: test\nwindow: {\"kind\":\"edge\",\"edge\":\"left\",\"autofocus\":\"search\"}\n---\n```mix\nroot: {widget: \"column\", children: [\"search\"]}\nsearch: {widget: \"field\", value: \"\", on_change: \"filter\"}\n```\n",
        );
        // The cache after the final resize, rather than the cache discarded
        // by runtime.resize(). This is the real widget and scene event route.
        let mut element = ui.build_view::<LayoutRenderer>();
        let mut state = iced_core::widget::Tree::empty();
        state.diff(element.as_widget_mut());
        let layout = element.as_widget_mut().layout(
            &mut state,
            &renderer,
            &iced_core::layout::Limits::new(
                iced_core::Size::ZERO,
                iced_core::Size::new(440.0, 812.0),
            ),
        );
        let mut focus = FocusField::new(autofocus(&ui.content.tree).unwrap());
        element.as_widget_mut().operate(
            &mut state,
            iced_core::Layout::new(&layout),
            &renderer,
            &mut focus,
        );
        assert!(focus.found, "search must map to field:search, not n:search");
        drop(element);
        // Restyle the same field with a larger prepared font, then reconcile
        // against its existing iced tree. Typing below must still reach it.
        ui.update(SceneMessage::Appearance(crate::appearance::fixture(
            "dark", 1.5,
        )));
        let mut element = ui.build_view::<LayoutRenderer>();
        state.diff(element.as_widget_mut());
        let layout = element.as_widget_mut().layout(
            &mut state,
            &renderer,
            &iced_core::layout::Limits::new(
                iced_core::Size::ZERO,
                iced_core::Size::new(440.0, 812.0),
            ),
        );
        let mut bus = iced_core::shell::Bus::new();
        let mut shell = iced_core::Shell::new(
            &iced_core::window::Headless,
            iced_core::shell::Waker::new(|| {}),
            &mut bus,
        );
        let event = iced_core::Event::Keyboard(iced_core::keyboard::Event::KeyPressed {
            key: iced_core::keyboard::Key::Character("a".into()),
            modified_key: iced_core::keyboard::Key::Character("a".into()),
            physical_key: iced_core::keyboard::key::Physical::Unidentified(
                iced_core::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced_core::keyboard::Location::Standard,
            modifiers: iced_core::keyboard::Modifiers::empty(),
            text: Some("a".into()),
            repeat: false,
        });
        element.as_widget_mut().update(
            &mut state,
            &event,
            iced_core::Layout::new(&layout),
            iced_core::mouse::Cursor::Unavailable,
            &renderer,
            &mut shell,
            &iced_core::Rectangle::with_size(iced_core::Size::new(440.0, 812.0)),
        );
        drop(shell);
        let messages: Vec<_> = bus.drain().collect();
        assert!(messages.iter().any(|message| {
            event_of(message)
                .is_some_and(|(_, verb, body)| verb == "filter" && body["value"] == "a")
        }));
    }

    #[test]
    fn every_calendar_node_has_visible_non_zero_bounds_including_the_footer() {
        let renderer = LayoutRenderer::new();
        let tree = scene::resolve(
            &scene::parse(include_str!("../tests/fixtures/calendar.scene.mix")).unwrap(),
        )
        .unwrap();
        let palette = decor::ChromeTheme::from_source(decor::ChromeStyle::Mac, None).palette;
        let design = compile_scene_design(design::EMBEDDED_DEFAULT_SOURCE, false).unwrap();
        let ui = SceneUi::with_design(
            Arc::new(Content {
                tree,
                lists: PreparedLists::new(),
                revision: 1,
                frame: None,
                dialog: false,
            }),
            palette,
            || design,
        );
        let mut state = iced_core::widget::Tree::empty();
        // Reuse the state as the native instance does when rebuilding or
        // resizing. Non-zero alone misses a footer pushed below the viewport.
        for (width, height) in [(360.0, 600.0), (380.0, 812.0), (360.0, 600.0)] {
            let mut element = ui.build_view::<LayoutRenderer>();
            state.diff(element.as_widget_mut());
            let layout = element.as_widget_mut().layout(
                &mut state,
                &renderer,
                &iced_core::layout::Limits::new(
                    iced_core::Size::ZERO,
                    iced_core::Size::new(width, height),
                ),
            );
            let mut measure = crate::layout::Measure::default();
            element.as_widget_mut().operate(
                &mut state,
                iced_core::Layout::new(&layout),
                &renderer,
                &mut measure,
            );
            for id in ui.content.tree.nodes.keys() {
                let bounds = measure
                    .found
                    .get(&node_id(id))
                    .unwrap_or_else(|| panic!("missing calendar node {id}"));
                assert!(
                    bounds.width > 0.0 && bounds.height > 0.0,
                    "{id}: {bounds:?} at {width}x{height}"
                );
                assert!(
                    bounds.x >= 0.0
                        && bounds.y >= 0.0
                        && bounds.x + bounds.width <= width + 0.1
                        && bounds.y + bounds.height <= height + 0.1,
                    "calendar node {id} outside viewport: {bounds:?} at {width}x{height}"
                );
            }
            let footer = measure.found[&node_id("cal_open")];
            let fill = measure.found[&node_id("cal_fill")];
            let last_week = measure.found[&node_id("wk_5")];
            assert!(last_week.y + last_week.height <= fill.y + 0.1);
            assert!(fill.y + fill.height < footer.y);
            assert!(
                (footer.y + footer.height - (height - 18.0)).abs() < 0.1,
                "footer must stay above the bottom padding"
            );
        }
    }

    #[test]
    fn scene_buttons_keep_quoin_design_pairs_for_every_tone_and_state() {
        use design::{DesignCompileResult, DesignContext, Mode, SourceIdentity};

        let tree = scene::resolve(
            &scene::parse(include_str!("../tests/fixtures/colours.scene.mix")).unwrap(),
        )
        .unwrap();
        let document = design::parse_design_source(
            SourceIdentity::new("test:scene-buttons"),
            design::EMBEDDED_DEFAULT_SOURCE,
        )
        .unwrap();
        for scheme in design::Scheme::ALL {
            for mode in Mode::ALL {
                let source = format!(
                    "{{scheme: \"{}\", mode: \"{}\"}}",
                    scheme.name(),
                    mode.name()
                );
                // Edges compile dark cells even with a light chrome selection;
                // dialog buttons compile the selected mode, as CTK does.
                for dialog in [false, true] {
                    let design = compile_scene_design(&source, dialog).unwrap();
                    let context = DesignContext {
                        scheme,
                        mode: if dialog { mode } else { Mode::Dark },
                        ..DesignContext::default()
                    };
                    let DesignCompileResult::Success(expected) =
                        design::compile_design(&document, context)
                    else {
                        panic!("embedded design compiles");
                    };
                    for (tone, variant) in [
                        ("primary", ButtonVariant::Primary),
                        ("danger", ButtonVariant::Destructive),
                        ("", ButtonVariant::Default),
                    ] {
                        for (status, interaction) in [
                            (button::Status::Active, InteractionState::Resting),
                            (button::Status::Hovered, InteractionState::Hovered),
                            (button::Status::Pressed, InteractionState::Pressed),
                            (button::Status::Disabled, InteractionState::Disabled),
                        ] {
                            let cell = expected.candidate.tables().button.cell(ButtonCellKey {
                                variant,
                                size: ButtonSize::Md,
                                interaction,
                                focus_visible: false,
                            });
                            // The schema admits tone alone. If a runtime node
                            // carries extra colour ports, CTK still owns its pair.
                            for extra_ports in [false, true] {
                                let mut node = tree.nodes["cal_open"].clone();
                                node.ports.insert("tone".into(), json!(tone));
                                if extra_ports {
                                    node.ports.insert("background".into(), json!("#202326f2"));
                                    node.ports.insert("hover".into(), json!("#ffffff1a"));
                                    node.ports.insert("color".into(), json!("#fcfcfc"));
                                }
                                let style = scene_button_style(&design.buttons, &node, status);
                                assert_eq!(
                                    style.background,
                                    Some(Background::Color(design_color(cell.pair.surface)))
                                );
                                assert_eq!(style.text_color, design_color(cell.pair.foreground));
                                // The rendered pair clears AA; no foreground
                                // is inferred from an independently chosen fill.
                                let fill = design_color(cell.pair.rendered_surface);
                                let ink = design_color(cell.pair.rendered_foreground);
                                assert!(
                                    fill.relative_contrast(ink) >= 4.49,
                                    "{} {} {tone} {status:?}",
                                    scheme.name(),
                                    mode.name()
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn scene_row_fills_preserve_the_bottom_panel_overlay_and_calendar_hover() {
        let tree = scene::resolve(
            &scene::parse(include_str!("../tests/fixtures/colours.scene.mix")).unwrap(),
        )
        .unwrap();
        let design = compile_scene_design("{scheme: \"ocean\", mode: \"light\"}", false).unwrap();
        let ink = color(design.palette.base.foreground);
        let style = |id: &str, status| scene_row_style(&tree.nodes[id], ink, status);
        assert_eq!(
            style("root", button::Status::Active).background,
            None,
            "the calendar reveals PANEL"
        );
        let grey = hex("#202326f2").unwrap();
        assert_eq!(
            style("panel", button::Status::Active).background,
            Some(Background::Color(grey))
        );
        assert_ne!(
            grey,
            color(design.palette.base.surface),
            "the bottom scene overlays PANEL"
        );
        assert_eq!(
            style("cal_today", button::Status::Active).background,
            hex("#ffffff0f").map(Background::Color)
        );
        for status in [button::Status::Hovered, button::Status::Pressed] {
            assert_eq!(
                style("cal_today", status).background,
                hex("#ffffff1a").map(Background::Color)
            );
            assert_eq!(style("cal_today", status).text_color, ink);
        }
        assert_eq!(port_text(&tree.nodes["cal_today_t"], "color"), "#fcfcfc");
    }

    #[test]
    fn a_design_button_mapping_override_replaces_both_halves_of_the_pair() {
        let rule = "compoundVariants: [";
        let source = design::EMBEDDED_DEFAULT_SOURCE;
        assert_eq!(source.matches(rule).count(), 1);
        // The compiler checks focused cells too: changing the primary variant's
        // pair alone leaves its focus_ring bound to primary (ring-surface-binding).
        // Override only the unfocused resting cell exercised by this scene.
        let overridden = source.replace(
            rule,
            r#"compoundVariants: [
          {
            when: { variant: "primary", size: "md", interaction: "resting", focus_visible: false },
            set: { pair: { kind: "pair", value: "secondary" } }
          },"#,
        );
        let design = compile_scene_design(&overridden, false).unwrap();
        let original = compile_scene_design(source, false).unwrap();
        let tree = scene::resolve(
            &scene::parse(include_str!("../tests/fixtures/colours.scene.mix")).unwrap(),
        )
        .unwrap();
        let node = &tree.nodes["cal_open"];
        let style = scene_button_style(&design.buttons, node, button::Status::Active);
        let key = ButtonCellKey {
            variant: ButtonVariant::Default,
            size: ButtonSize::Md,
            interaction: InteractionState::Resting,
            focus_visible: false,
        };
        let expected = original.buttons.cell(key);
        assert_eq!(
            style.background,
            Some(Background::Color(design_color(expected.pair.surface)))
        );
        assert_eq!(style.text_color, design_color(expected.pair.foreground));
        let before = scene_button_style(&original.buttons, node, button::Status::Active);
        assert_ne!(style.background, before.background);
        assert_ne!(style.text_color, before.text_color);
    }

    #[test]
    fn scene_colours_parse_as_quoin_parses_them() {
        assert_eq!(hex("#fff"), Some(Color::from_rgba8(255, 255, 255, 1.0)));
        assert_eq!(
            hex("#20242d"),
            Some(Color::from_rgba8(0x20, 0x24, 0x2d, 1.0))
        );
        assert_eq!(hex("#00000080").map(|c| (c.a * 255.0).round()), Some(128.0));
        for bad in ["", "fff", "#ff", "#gggggg", "#ffffffff0", "#ééé"] {
            assert_eq!(hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn events_carry_the_quoin_payload_and_only_bound_handlers_fire() {
        let tree = scene::resolve(&scene::parse(scene::fixtures::CONFORMANCE).unwrap()).unwrap();
        let button = Arc::new(Route::new(&tree, "button", &tree.nodes["button"]));
        let click = SceneMessage::Click(Arc::clone(&button));
        let (citizen, handler, body) = event_of(&click).unwrap();
        assert_eq!((citizen, handler), ("conformance-citizen", "go"));
        assert_eq!(
            body,
            json!({"scene":"conformance","node":"button","kind":"click"})
        );
        // The button has no change handler.
        assert!(event_of(&SceneMessage::Toggle(button, "k".into(), true)).is_none());
        let field = Arc::new(Route::new(&tree, "field", &tree.nodes["field"]));
        let submit = SceneMessage::Submit(Arc::clone(&field), "typed".into());
        let (_, handler, body) = event_of(&submit).unwrap();
        assert_eq!((handler, body["value"].as_str()), ("submit", Some("typed")));
        let mut row = Route::new(&tree, "list", &tree.nodes["list"]);
        row.item = Some(json!({"id":"1","cells":["one"]}));
        let row_click = SceneMessage::Click(Arc::new(row));
        let (_, handler, body) = event_of(&row_click).unwrap();
        assert_eq!(handler, "select");
        assert_eq!(
            body,
            json!({"scene":"conformance","node":"list","kind":"click","item":{"id":"1","cells":["one"]}})
        );
    }

    #[test]
    fn scenes_use_their_design_page_pair_and_report_it_as_hex() {
        let tree = scene::resolve(&scene::parse(scene::fixtures::CONFORMANCE).unwrap()).unwrap();
        let palette = decor::ChromeTheme::from_source(decor::ChromeStyle::Mac, None).palette;
        let edge = compile_scene_design(design::EMBEDDED_DEFAULT_SOURCE, false).unwrap();
        for (frame, dialog) in [
            (None, false),
            (None, true),
            (Some(Frame { title: "t".into() }), true),
        ] {
            let ui = SceneUi::with_design(
                Arc::new(Content {
                    tree: tree.clone(),
                    lists: PreparedLists::new(),
                    revision: 1,
                    frame,
                    dialog,
                }),
                palette,
                || edge.clone(),
            );
            let pair = if dialog {
                palette.base
            } else {
                edge.palette.base
            };
            assert_eq!(
                (ui.page(), ui.ink()),
                (color(pair.surface), color(pair.foreground))
            );
        }
        assert_eq!(
            hex_of(Color::from_rgba8(0x20, 0x23, 0x26, 1.0)),
            "#202326ff"
        );
        assert_eq!(
            hex(&hex_of(color(palette.base.surface))).map(hex_of),
            Some(hex_of(color(palette.base.surface)))
        );
    }

    #[test]
    fn right_and_bottom_edge_pages_share_quoin_dark_panel_pair_with_light_chrome() {
        let palette = decor::ChromeTheme::from_source(
            decor::ChromeStyle::Mac,
            Some("{scheme: \"ocean\", mode: \"light\"}"),
        )
        .palette;
        let edge_design =
            compile_scene_design("{scheme: \"ocean\", mode: \"light\"}", false).unwrap();
        let expected = decor::ChromeTheme::from_source(
            decor::ChromeStyle::Mac,
            Some("{scheme: \"ocean\", mode: \"dark\"}"),
        )
        .palette
        .secondary;
        let ui = |edge: &str| {
            let source = format!(
                "---\nscene: 1\nname: page\ncitizen: c\nwindow: {{\"kind\":\"edge\",\"edge\":\"{edge}\"}}\n---\n```mix\nroot: {{widget: \"column\", fill: true, children: []}}\n```\n"
            );
            let tree = scene::resolve(&scene::parse(&source).unwrap()).unwrap();
            assert_eq!(crate::mount::scene_edge(&tree).as_str(), edge);
            SceneUi::with_design(
                Arc::new(Content {
                    tree,
                    lists: PreparedLists::new(),
                    revision: 1,
                    frame: None,
                    dialog: false,
                }),
                palette,
                || edge_design.clone(),
            )
        };
        let (right, bottom) = (ui("right"), ui("bottom"));
        assert!(right.theme.palette().is_dark && bottom.theme.palette().is_dark);
        assert_eq!(right.page(), bottom.page());
        assert_eq!(hex_of(right.page()), hex_of(bottom.page()));
        assert_eq!(
            (right.page(), right.ink()),
            (color(expected.surface), color(expected.foreground))
        );
        assert_ne!(
            right.page(),
            color(palette.base.surface),
            "light chrome cannot supply the edge page"
        );
    }

    #[test]
    fn edge_palette_forces_dark_tokens_and_preserves_the_selected_scheme() {
        for scheme in design::Scheme::ALL {
            let dark = format!("{{scheme: \"{}\", mode: \"dark\"}}", scheme.name());
            let expected =
                decor::ChromeTheme::from_source(decor::ChromeStyle::Mac, Some(&dark)).palette;
            for mode in ["light", "dark"] {
                let source = format!("{{scheme: \"{}\", mode: \"{mode}\"}}", scheme.name());
                let actual = compile_scene_design(&source, false).unwrap().palette;
                assert_eq!(actual.base, expected.secondary, "{} {mode}", scheme.name());
                assert_eq!(actual.primary, expected.primary);
            }
        }
        assert!(compile_scene_design("not a design", false).is_none());
    }

    #[test]
    fn escape_routes_to_the_focused_dialog_or_edge() {
        let tree = scene::resolve(&scene::parse(scene::fixtures::CONFORMANCE).unwrap()).unwrap();
        let palette = decor::ChromeTheme::from_source(decor::ChromeStyle::Mac, None).palette;
        let ui = |dialog: bool| {
            SceneUi::new(
                Arc::new(Content {
                    tree: tree.clone(),
                    lists: PreparedLists::new(),
                    revision: 1,
                    frame: None,
                    dialog,
                }),
                palette,
            )
        };
        let press = |key: iced_core::keyboard::Key| {
            iced_core::Event::Keyboard(iced_core::keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key,
                physical_key: iced_core::keyboard::key::Physical::Unidentified(
                    iced_core::keyboard::key::NativeCode::Unidentified,
                ),
                location: iced_core::keyboard::Location::Standard,
                modifiers: iced_core::keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            })
        };
        let escape = press(iced_core::keyboard::Key::Named(
            iced_core::keyboard::key::Named::Escape,
        ));
        let (dialog, edge) = (ui(true), ui(false));
        assert_eq!(dialog.subscribe(), EventFlags::KEYBOARD);
        assert!(matches!(
            dialog.event_process(&escape).as_slice(),
            [SceneMessage::Close]
        ));
        assert!(
            dialog
                .event_process(&press(iced_core::keyboard::Key::Character("a".into())))
                .is_empty()
        );
        assert_eq!(edge.subscribe(), EventFlags::KEYBOARD | EventFlags::WINDOW);
        assert!(matches!(
            edge.event_process(&escape).as_slice(),
            [SceneMessage::EscapeEdge]
        ));
        assert!(matches!(
            edge.event_process(&iced_core::Event::Window(
                iced_core::window::Event::Unfocused
            ))
            .as_slice(),
            [SceneMessage::EdgeFocus(false)]
        ));
    }

    #[test]
    fn spacers_and_explicit_lengths_survive_the_measuring_wrapper() {
        let tree = scene::resolve(&scene::parse(scene::fixtures::CONFORMANCE).unwrap()).unwrap();
        let mut node = tree.nodes["root"].clone();
        node.family = "spacer".into();
        node.ports.clear();
        let row = Parent {
            main: Axis::Horizontal,
            stretch: false,
        };
        let column = Parent {
            main: Axis::Vertical,
            stretch: false,
        };
        assert_eq!(SceneUi::sizing(&node, row), (Length::Fill, Length::Shrink));
        assert_eq!(
            SceneUi::sizing(&node, column),
            (Length::Shrink, Length::Fill)
        );
        node.ports.insert("size".into(), json!(6));
        assert_eq!(
            SceneUi::sizing(&node, row),
            (Length::Fixed(6.0), Length::Fixed(6.0))
        );
        node.family = "column".into();
        node.ports.clear();
        node.ports.insert("width".into(), json!("fill"));
        node.ports.insert("height".into(), json!(40));
        assert_eq!(
            SceneUi::sizing(&node, row),
            (Length::Fill, Length::Fixed(40.0))
        );
        node.family = "list".into();
        node.ports.clear();
        node.ports.insert("fill".into(), json!(true));
        assert_eq!(SceneUi::sizing(&node, column).1, Length::Fill);
        node.ports.clear();
        node.ports.insert("grow".into(), json!(1));
        assert_eq!(SceneUi::sizing(&node, column).1, Length::FillPortion(1));
    }

    #[test]
    fn autofocus_only_lands_on_a_reachable_visible_field() {
        let source = "---\nscene: 1\nname: auto-test\ncitizen: c\nwindow: {\"kind\":\"edge\",\"edge\":\"left\",\"autofocus\":\"search\"}\n---\n```mix\nroot: {widget: \"column\", children: [\"search\"]}\nsearch: {widget: \"field\", value: \"\"}\n```\n";
        let mut tree = scene::resolve(&scene::parse(source).unwrap()).unwrap();
        assert_eq!(autofocus(&tree), Some("search"));
        tree.nodes
            .get_mut("root")
            .unwrap()
            .ports
            .insert("hidden".into(), json!(true));
        assert_eq!(autofocus(&tree), None);
        tree.nodes
            .get_mut("root")
            .unwrap()
            .ports
            .shift_remove("hidden");
        tree.nodes.get_mut("search").unwrap().family = "text".into();
        assert_eq!(autofocus(&tree), None);
    }

    #[test]
    fn a_list_shows_at_most_max_rows_before_it_scrolls() {
        let tree = scene::resolve(&scene::parse(scene::fixtures::CLIPPANEL).unwrap()).unwrap();
        // table: 2 rows, row_height 38, gap 3, max_rows unset (8).
        assert_eq!(list_height(&tree.nodes["table"]), 2.0 * 41.0);
        // remote: no rows still keeps one row's height.
        assert_eq!(list_height(&tree.nodes["remote"]), 33.0);
    }
}
