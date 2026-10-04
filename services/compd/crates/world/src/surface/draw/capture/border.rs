//! `RegionBorder` — the bright outline around the captured region. Drawn above
//! everything (screen-space, top layer). Fully transparent except the outline,
//! and click-through (the interface adds the passthrough layer bit).

use iced_core::{Color, Element, Theme};
use crate::capture::session::message::{CaptureMessage, OverlayRect};
use ui::engine::{IcedUi, Renderer};

use crate::surface::draw::capture::{mask, style};

pub struct RegionBorder {
    pub screen_w: i32,
    pub screen_h: i32,
    pub rect: OverlayRect,
}

impl RegionBorder {
    pub fn new(screen_w: i32, screen_h: i32, rect: OverlayRect) -> Self {
        Self {
            screen_w,
            screen_h,
            rect,
        }
    }
}

impl IcedUi for RegionBorder {
    type Message = CaptureMessage;

    fn update(&mut self, message: Self::Message) {
        if let CaptureMessage::SetRegion(r) = message {
            self.rect = r;
        }
    }

    fn view(&self) -> Element<'_, Self::Message, Theme, Renderer> {
        // Inflate by the border width so the stroke lands entirely OUTSIDE the
        // captured region (otherwise the border pixels show up in the capture).
        // `region_border` draws the four edges independently and crops any edge
        // that pans off-screen — on every side, not just bottom/right.
        let bw = style::BORDER_WIDTH as i32;
        let outer = OverlayRect {
            x: self.rect.x - bw,
            y: self.rect.y - bw,
            w: self.rect.w + 2 * bw,
            h: self.rect.h + 2 * bw,
        };
        mask::region_border(self.screen_w, self.screen_h, outer, style::ACCENT, bw)
    }
}
