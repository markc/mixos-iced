//! Builds a full-screen mask with an optional rectangular "hole".
//!
//! Implemented with plain flexbox widgets (no canvas feature): four solid
//! bands around the hole, and the hole itself a transparent cell that can carry
//! a border. The mask color may be fully transparent (used by the border-only
//! overlay).

use iced_core::{Background, Border, Color, Element, Length, Shadow, Theme};
use iced_widget::{Space, column, container, row};
use crate::capture::session::message::{CaptureMessage, OverlayRect};
use ui::engine::Renderer;

type El<'a> = Element<'a, CaptureMessage, Theme, Renderer>;

fn solid<'a>(w: Length, h: Length, color: Color) -> El<'a> {
    container(Space::new())
        .width(w)
        .height(h)
        .style(move |_t| container::Style {
            background: Some(Background::Color(color)),
            text_color: None,
            border: Border::default(),
            shadow: Shadow::default(),
            snap: true,
        })
        .into()
}

fn hole_cell<'a>(w: Length, h: Length, border: Option<(Color, f32)>) -> El<'a> {
    let (bc, bw) = border.unwrap_or((Color::TRANSPARENT, 0.0));
    container(Space::new())
        .width(w)
        .height(h)
        .style(move |_t| container::Style {
            background: None,
            text_color: None,
            border: Border {
                color: bc,
                width: bw,
                radius: 0.0.into(),
            },
            shadow: Shadow::default(),
            snap: true,
        })
        .into()
}

/// A `screen_w` × `screen_h` mask. With `hole = Some(rect)` the rect area is
/// left clear (and optionally outlined); otherwise the whole area is masked.
pub fn mask_with_hole<'a>(
    screen_w: i32,
    screen_h: i32,
    hole: Option<OverlayRect>,
    mask: Color,
    border: Option<(Color, f32)>,
) -> El<'a> {
    let Some(h) = hole else {
        return solid(Length::Fill, Length::Fill, mask);
    };

    // Clip the hole to the screen by its EDGES (clamping `x` alone, without
    // shrinking the width, would shift an off-left hole to x=0 keeping full
    // width — the wrong position).
    let left = h.x.clamp(0, screen_w);
    let right = (h.x + h.w).clamp(0, screen_w);
    let top = h.y.clamp(0, screen_h);
    let bottom = (h.y + h.h).clamp(0, screen_h);
    let x = left;
    let y = top;
    let w = (right - left).max(0);
    let ht = (bottom - top).max(0);
    let right_w = (screen_w - right).max(0);
    let bottom_h = (screen_h - bottom).max(0);

    let middle = row![
        solid(Length::Fixed(x as f32), Length::Fixed(ht as f32), mask),
        hole_cell(Length::Fixed(w as f32), Length::Fixed(ht as f32), border),
        solid(Length::Fixed(right_w as f32), Length::Fixed(ht as f32), mask),
    ]
    .width(Length::Fill)
    .height(Length::Fixed(ht as f32));

    column![
        solid(Length::Fill, Length::Fixed(y as f32), mask),
        middle,
        solid(Length::Fill, Length::Fixed(bottom_h as f32), mask),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// A `bw`-thick rectangular outline of `rect`, drawn as four independent edge
/// bands. Each edge is drawn only when its true position is on-screen and is
/// clipped to the screen — so a region panned off any side has that side's edge
/// cropped (omitted) while the perpendicular edges keep their true positions.
pub fn region_border<'a>(
    screen_w: i32,
    screen_h: i32,
    rect: OverlayRect,
    color: Color,
    bw: i32,
) -> El<'a> {
    let clear = Color::TRANSPARENT;
    let left = rect.x;
    let top = rect.y;
    let right = rect.x + rect.w;
    let bottom = rect.y + rect.h;

    // Clipped extent of the outline on screen.
    let cx0 = left.clamp(0, screen_w);
    let cx1 = right.clamp(0, screen_w);
    let cy0 = top.clamp(0, screen_h);
    let cy1 = bottom.clamp(0, screen_h);
    let cw = (cx1 - cx0).max(0);
    let ch = (cy1 - cy0).max(0);
    if cw == 0 || ch == 0 {
        return solid(Length::Fill, Length::Fill, clear);
    }

    // Draw an edge only when its line is within the screen.
    let top_h = if (0..=screen_h).contains(&top) { bw } else { 0 };
    let bot_h = if (0..=screen_h).contains(&bottom) { bw } else { 0 };
    let left_w = if (0..=screen_w).contains(&left) { bw } else { 0 };
    let right_w = if (0..=screen_w).contains(&right) { bw } else { 0 };
    let mid_h = (ch - top_h - bot_h).max(0);

    // The outline box (cw × ch): top band, middle (left/right bands), bottom band.
    let bordered = column![
        solid(Length::Fixed(cw as f32), Length::Fixed(top_h as f32), color),
        row![
            solid(Length::Fixed(left_w as f32), Length::Fixed(mid_h as f32), color),
            solid(Length::Fill, Length::Fixed(mid_h as f32), clear),
            solid(Length::Fixed(right_w as f32), Length::Fixed(mid_h as f32), color),
        ]
        .width(Length::Fixed(cw as f32))
        .height(Length::Fixed(mid_h as f32)),
        solid(Length::Fixed(cw as f32), Length::Fixed(bot_h as f32), color),
    ]
    .width(Length::Fixed(cw as f32))
    .height(Length::Fixed(ch as f32));

    // Position the box at (cx0, cy0).
    column![
        solid(Length::Fill, Length::Fixed(cy0 as f32), clear),
        row![
            solid(Length::Fixed(cx0 as f32), Length::Fixed(ch as f32), clear),
            bordered,
            solid(Length::Fill, Length::Fixed(ch as f32), clear),
        ]
        .width(Length::Fill)
        .height(Length::Fixed(ch as f32)),
        solid(Length::Fill, Length::Fill, clear),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}
