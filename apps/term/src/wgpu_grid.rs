// SPDX-License-Identifier: MIT OR Apache-2.0
//! Adapter from the terminal raster to the shared native GPU painter.
use crate::frame::Frame;
pub use application::gpu_grid::GridProgram;
use application::gpu_grid::{DamageBand, FrameSource, GridId};

impl FrameSource for Frame {
    fn identity(&self) -> GridId {
        self.gpu_id.clone()
    }
    fn dimensions(&self) -> (u32, u32) {
        (self.surface().width(), self.surface().height())
    }
    fn stride(&self) -> usize {
        self.surface().stride()
    }
    fn pixels(&self) -> &[u8] {
        self.surface().rgba()
    }
    fn clear_damage(&mut self) {
        Frame::clear_damage(self);
    }
    fn take_damage(&mut self) -> Vec<DamageBand> {
        Frame::take_damage(self)
            .into_iter()
            .map(|band| DamageBand {
                x: band.x,
                y: band.y,
                width: band.width,
                height: band.height,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use application::gpu_grid::upload_region;
    use application::iced::wgpu;
    use term_core::raster::DamageBand;
    fn upload_region_for_core(
        stride: usize,
        band: DamageBand,
    ) -> (wgpu::Origin3d, wgpu::TexelCopyBufferLayout, wgpu::Extent3d) {
        upload_region(
            stride,
            application::gpu_grid::DamageBand {
                x: band.x,
                y: band.y,
                width: band.width,
                height: band.height,
            },
        )
    }

    use crate::frame::Painter;
    use term_core::{
        config::Cursor,
        font::FontSize,
        terminal::{Cell, Screen},
    };

    #[test]
    fn range_upload_layout_repairs_a_texture_after_coalesced_paints() {
        let mut painter = Painter::for_test(1.25, FontSize::new(13.0), Cursor::Block).unwrap();
        let mut screen = Screen {
            clusters: Default::default(),
            cols: 90,
            rows: 6,
            display_offset: 0,
            cursor: (1, 0),
            cursor_visible: true,
            cells: vec![
                Cell {
                    extra: 0,
                    width: Default::default(),
                    c: 'M',
                    fg: [201, 31, 127],
                    bg: [9, 17, 32],
                    bold: false
                };
                540
            ],
            updated: std::time::Instant::now(),
        };
        painter.repaint(1, &screen, &[]);
        let shared = painter.frame(1);
        let mut texture = shared.lock().unwrap().surface().rgba().to_vec();
        shared.lock().unwrap().clear_damage();
        for (col, row) in [(80, 3), (5, 3), (45, 5)] {
            screen.cells[row * 90 + col].c = 'g';
            screen.cursor = (col, row);
            let mut dirty = [false; 6];
            dirty[row] = true;
            painter.repaint(1, &screen, &dirty);
        }
        let mut frame = shared.lock().unwrap();
        let stride = frame.surface().stride();
        for band in frame.take_damage() {
            assert!(band.width < frame.surface().width());
            let (origin, layout, extent) = upload_region_for_core(stride, band);
            for row in 0..extent.height as usize {
                let source = layout.offset as usize + row * layout.bytes_per_row.unwrap() as usize;
                let target = (origin.y as usize + row) * stride + origin.x as usize * 4;
                let len = extent.width as usize * 4;
                texture[target..target + len]
                    .copy_from_slice(&frame.surface().rgba()[source..source + len]);
            }
        }
        assert_eq!(texture, frame.surface().rgba());
        assert!(frame.take_damage().is_empty());
    }

    #[test]
    fn emoji_spacer_damage_uploads_both_columns() {
        let mut painter = Painter::for_test(1.25, FontSize::new(13.0), Cursor::Block).unwrap();
        let mut screen =
            term_core::terminal::Terminal::from_test_vt(8, 3, " 👩‍💻".as_bytes()).screen(false);
        screen.cursor_visible = false;
        painter.repaint(1, &screen, &[]);
        let shared = painter.frame(1);
        let mut texture = shared.lock().unwrap().surface().rgba().to_vec();
        shared.lock().unwrap().clear_damage();
        screen.cells[2].bg = [20, 90, 150];
        painter.repaint(1, &screen, &[true, false, false]);
        let mut frame = shared.lock().unwrap();
        let stride = frame.surface().stride();
        let damage = frame.take_damage();
        assert_eq!(damage.len(), 1);
        assert_eq!(damage[0].x, painter.cell().0);
        assert_eq!(damage[0].width, painter.cell().0 * 2);
        for band in damage {
            let (origin, layout, extent) = upload_region_for_core(stride, band);
            for row in 0..extent.height as usize {
                let source = layout.offset as usize + row * layout.bytes_per_row.unwrap() as usize;
                let target = (origin.y as usize + row) * stride + origin.x as usize * 4;
                let len = extent.width as usize * 4;
                texture[target..target + len]
                    .copy_from_slice(&frame.surface().rgba()[source..source + len]);
            }
        }
        assert_eq!(texture, frame.surface().rgba());
    }
}
