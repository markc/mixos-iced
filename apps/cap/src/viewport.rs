// SPDX-License-Identifier: MIT OR Apache-2.0
//! One image-pixel transform for painting and pointer mapping.
use crate::document::Point;
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    pub scale: f32,
    pub origin: Point,
    pub width: f32,
    pub height: f32,
}
impl Viewport {
    pub fn fit(image: (u32, u32), view: (f32, f32), zoom: f32, pan: Point) -> Self {
        let scale = (view.0 / image.0 as f32)
            .min(view.1 / image.1 as f32)
            .max(0.0001)
            * zoom.clamp(0.1, 8.0);
        let width = image.0 as f32 * scale;
        let height = image.1 as f32 * scale;
        Self {
            scale,
            origin: Point {
                x: (view.0 - width) / 2.0 + pan.x,
                y: (view.1 - height) / 2.0 + pan.y,
            },
            width,
            height,
        }
    }
    pub fn image(self, p: Point) -> Point {
        Point {
            x: (p.x - self.origin.x) / self.scale,
            y: (p.y - self.origin.y) / self.scale,
        }
    }
    pub fn view(self, p: Point) -> Point {
        Point {
            x: p.x * self.scale + self.origin.x,
            y: p.y * self.scale + self.origin.y,
        }
    }
    pub fn contains(self, p: Point) -> bool {
        let p = self.image(p);
        p.x >= 0.0
            && p.y >= 0.0
            && p.x <= self.width / self.scale
            && p.y <= self.height / self.scale
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fractional_zoom_and_pan_are_invertible() {
        let v = Viewport::fit(
            (3840, 2160),
            (911.0, 517.0),
            1.7,
            Point { x: 11.5, y: -3.0 },
        );
        let p = Point { x: 101.5, y: 777.0 };
        let q = v.image(v.view(p));
        assert!((q.x - p.x).abs() < 0.001 && (q.y - p.y).abs() < 0.001);
        assert!(!v.contains(Point {
            x: -10000.0,
            y: 0.0
        }));
    }
}
