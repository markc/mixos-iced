// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

/// Four terminal rows bound retained-buffer copies on echo (damage is per cell),
/// without creating a widget for every physical scanline.
pub(super) const ROWS_PER_BAND: usize = 4;

#[derive(Default)]
pub struct Surface {
    pub(super) tiles: Vec<PixelBand>,
    bands: Vec<DamageBand>,
    width: u32,
    height: u32,
    cell: (u32, u32),
    #[cfg(test)]
    pub(super) disable_scroll: bool,
    #[cfg(test)]
    pub(super) prepared_cells: usize,
}

impl Surface {
    #[cfg(test)]
    pub fn width(&self) -> u32 {
        self.width
    }
    #[cfg(test)]
    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub fn invalidate(&mut self) {
        for tile in &mut self.tiles {
            tile.invalidate();
        }
    }

    #[cfg(test)]
    // Preserve the shared Frame tests' RGBA oracle; runtime has no readback.
    pub fn rgba(&self) -> Vec<u8> {
        self.tiles
            .iter()
            .flat_map(|tile| {
                tile.native
                    .chunks_exact(4)
                    .flat_map(|p| [p[2], p[1], p[0], p[3]])
            })
            .collect()
    }

    #[cfg(test)]
    pub fn allocation(&self) -> *const u8 {
        self.tiles
            .first()
            .map_or(std::ptr::null(), |tile| tile.native.as_ptr())
    }

    pub fn paint(&mut self, raster: &mut Raster, screen: &Screen, dirty: &[bool]) -> &[DamageBand] {
        self.bands.clear();
        let (width, height) = raster.target_size(screen);
        let rows = height as usize / raster.height as usize;
        let shift = self.scroll_shift(raster, screen, dirty, width, height, rows);
        if (self.width, self.height, self.cell) != (width, height, (raster.width, raster.height)) {
            self.invalidate();
        }
        self.width = width;
        self.height = height;
        self.cell = (raster.width, raster.height);
        self.tiles
            .resize_with(rows.div_ceil(ROWS_PER_BAND), PixelBand::default);
        let valid_damage = rows == screen.rows && dirty.len() == rows;
        // Retain immutable source generations until every destination is done.
        let sources: Vec<_> = if shift.is_some() {
            self.tiles
                .iter()
                .map(|tile| (tile.native.clone(), tile.state.cursor_row()))
                .collect()
        } else {
            Vec::new()
        };
        for (index, tile) in self.tiles.iter_mut().enumerate() {
            let first = index * ROWS_PER_BAND;
            let end = (first + ROWS_PER_BAND).min(rows);
            // Like Ced's retained viewport rows, unchanged bands need no new
            // local snapshot. Include cursor/identity/geometry checks rather
            // than treating a clean damage hint as proof of valid pixels.
            if shift.is_none()
                && raster.is_current_rows(screen, &tile.state, first..end, dirty, PixelFormat::Bgra)
            {
                continue;
            }
            #[cfg(test)]
            {
                self.prepared_cells += (end - first) * screen.cols;
            }
            // Keep the original cell columns and translate only the row and
            // cursor origin. Core PaintState then erases the old cursor even
            // when it moves to another band or disappears on a resize.
            let part = Screen {
                clusters: screen.clusters.clone(),
                cols: screen.cols,
                rows: end - first,
                cursor: (screen.cursor.0, screen.cursor.1.saturating_sub(first)),
                cursor_visible: screen.cursor_visible && (first..end).contains(&screen.cursor.1),
                display_offset: screen.display_offset,
                cells: screen.cells[first * screen.cols..end * screen.cols].to_vec(),
                updated: screen.updated,
            };
            let dirty = if valid_damage {
                &dirty[first..end]
            } else {
                &[]
            };
            let damage = if let Some(shift) = shift {
                tile.paint_scrolled(raster, &part, first, shift, rows, &sources)
            } else {
                tile.paint_inner(raster, &part, dirty)
            };
            for damage in damage {
                self.bands.push(DamageBand {
                    x: damage.x,
                    width: damage.width,
                    y: first as u32 * raster.height + damage.y,
                    height: damage.height,
                });
            }
        }
        &self.bands
    }

    #[allow(clippy::too_many_arguments)]
    fn scroll_shift(
        &self,
        raster: &Raster,
        screen: &Screen,
        dirty: &[bool],
        width: u32,
        height: u32,
        rows: usize,
    ) -> Option<isize> {
        #[cfg(test)]
        if self.disable_scroll {
            return None;
        }
        if rows < 3
            || rows != screen.rows
            || dirty.len() != rows
            || dirty.iter().any(|&row| !row)
            || (self.width, self.height, self.cell)
                != (width, height, (raster.width, raster.height))
            || self.tiles.len() != rows.div_ceil(ROWS_PER_BAND)
        {
            return None;
        }
        // Charge every exact row check, including cheap failed anchors, to
        // one shared budget. Repetitive redraws must not spend sixteen
        // screenfuls comparing text before starting their ordinary paint.
        let remaining = std::cell::Cell::new(rows.saturating_mul(2));
        let matches = |old: usize, new: usize| {
            if remaining.get() == 0 {
                return false;
            }
            remaining.set(remaining.get() - 1);
            raster.matches_cached_row(
                screen,
                &self.tiles[old / ROWS_PER_BAND].state,
                old % ROWS_PER_BAND,
                new,
                PixelFormat::Bgra,
            )
        };
        // Echo and cursor movement belong on the tiny-damage path. Relocate
        // only when most visible rows actually changed, not merely dirty hints.
        if (0..rows)
            .filter(|&row| dirty[row] && !matches(row, row))
            .take(rows / 2 + 1)
            .count()
            <= rows / 2
        {
            return None;
        }
        // Prefer small shifts and retain at least half the viewport. Exact
        // row checks make ambiguous duplicate/blank rows harmless.
        // Anchors reject most candidates cheaply. The complete detector,
        // including its initial positional comparisons, checks at most two
        // screenfuls of rows; exhaustion falls back to ordinary painting.
        for amount in 1..=rows / 2 {
            for shift in [amount as isize, -(amount as isize)] {
                if remaining.get() == 0 {
                    return None;
                }
                let first = if shift < 0 { amount } else { 0 };
                let end = if shift > 0 { rows - amount } else { rows };
                if !matches((first as isize + shift) as usize, first)
                    || !matches(((end - 1) as isize + shift) as usize, end - 1)
                {
                    continue;
                }
                // The endpoints are already verified; do not charge or check
                // them twice, particularly in a small viewport.
                if (first + 1..end - 1).all(|new| matches((new as isize + shift) as usize, new)) {
                    return Some(shift);
                }
            }
        }
        None
    }

    pub fn cache_handle(&mut self, generation: u64) {
        for tile in &mut self.tiles {
            // paint releases the handle only for a potentially changed band.
            // Unchanged bands MUST keep their id across pane generations.
            if tile.cached.is_none() {
                tile.cache_handle(generation);
            }
        }
    }

    pub(super) fn images(&self, scale: f32) -> Vec<(Handle, application::iced::Rectangle)> {
        let mut y = 0;
        self.tiles
            .iter()
            .filter_map(|tile| {
                let top = y as f32 / scale;
                y += tile.height;
                let bottom = y as f32 / scale;
                let bounds = application::iced::Rectangle {
                    x: 0.0,
                    y: top,
                    width: tile.width as f32 / scale,
                    height: bottom - top,
                };
                tile.cached
                    .as_ref()
                    .map(|(_, handle)| (handle.clone(), bounds))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use term_core::{config::Cursor, terminal::Cell};

    #[test]
    fn retained_bands_prepare_only_changed_rows_and_repair_wide_cursors() {
        for scale in [1.0, 1.25, 2.5] {
            let mut raster = Raster::for_test(scale, 13.0, Cursor::Block).unwrap();
            let mut surface = Surface::default();
            let mut screen = term_core::terminal::Terminal::from_test_vt(
                13,
                11,
                "\x1b[4;2H界\x1b[5;2H👍🏽".as_bytes(),
            )
            .screen(false);
            screen.cursor = (2, 3); // Wide-cell spacer: cursor covers its leader.
            screen.cursor_visible = true;
            surface.paint(&mut raster, &screen, &[]);
            surface.cache_handle(1);
            let retained = surface.images(scale);
            let old: Vec<_> = retained.iter().map(|(h, _)| h.pixels().to_vec()).collect();
            for dirty in [[false; 11], [true; 11]] {
                surface.prepared_cells = 0;
                assert!(surface.paint(&mut raster, &screen, &dirty).is_empty());
                assert_eq!(surface.prepared_cells, 0, "unchanged band was copied");
            }
            screen.cells[0].bg = [71, 32, 93];
            let mut dirty = [false; 11];
            dirty[0] = true;
            surface.prepared_cells = 0;
            surface.paint(&mut raster, &screen, &dirty);
            assert_eq!(surface.prepared_cells, 4 * screen.cols);
            assert_eq!(surface.rgba(), raster.render(&screen));

            // Wide cursors cross a band boundary, disappear and reappear.
            for (cursor, visible, prepared_rows) in [
                ((2, 4), true, 8),
                ((2, 4), false, 4),
                ((2, 3), true, 4),
                ((0, 10), true, 7),
            ] {
                screen.cursor = cursor;
                screen.cursor_visible = visible;
                surface.prepared_cells = 0;
                surface.paint(&mut raster, &screen, &[false; 11]);
                assert_eq!(surface.prepared_cells, prepared_rows * screen.cols);
                assert_eq!(surface.rgba(), raster.render(&screen));
            }
            for invalidation in 0..4 {
                match invalidation {
                    0 => surface.invalidate(),
                    1 => screen.display_offset += 1,
                    2 => screen.clusters = Default::default(),
                    _ => raster = raster.resized(scale, 13.0).unwrap(),
                }
                surface.prepared_cells = 0;
                surface.paint(&mut raster, &screen, &[false; 11]);
                assert_eq!(surface.prepared_cells, screen.cols * screen.rows);
                assert_eq!(surface.rgba(), raster.render(&screen));
            }
            for ((handle, _), bytes) in retained.iter().zip(&old) {
                assert_eq!(handle.pixels().as_ref(), bytes.as_slice());
            }
        }
    }

    #[test]
    fn repetitive_redraw_with_late_mismatch_falls_back_to_exact_pixels() {
        let mut raster = Raster::for_test(1.0, 13.0, Cursor::Underline).unwrap();
        let mut surface = Surface::default();
        let mut screen = term_core::terminal::Terminal::from_test_vt(2, 513, b"").screen(false);
        screen.cursor_visible = false;
        for (row, cells) in screen.cells.chunks_mut(2).enumerate() {
            for cell in cells {
                cell.c = if row % 2 == 0 { 'A' } else { 'B' };
            }
        }
        surface.paint(&mut raster, &screen, &[]);
        for (row, cells) in screen.cells.chunks_mut(2).enumerate() {
            for cell in cells {
                cell.c = if row == 400 {
                    'X'
                } else if row % 2 == 0 {
                    'B'
                } else {
                    'A'
                };
            }
        }
        let dirty = vec![true; 513];
        assert_eq!(
            surface.scroll_shift(&raster, &screen, &dirty, surface.width, surface.height, 513),
            None
        );
        surface.paint(&mut raster, &screen, &dirty);
        assert_eq!(surface.rgba(), raster.render(&screen));
    }

    #[test]
    fn scrolling_reuses_pixels_across_bands_and_preserves_retained_generations() {
        for scale in [1.0, 1.25, 2.5] {
            for cursor in [Cursor::Block, Cursor::Underline] {
                let mut raster = Raster::for_test(scale, 13.0, cursor).unwrap();
                let mut surface = Surface::default();
                let mut screen = term_core::terminal::Terminal::from_test_vt(12, 11,
                    "\x1b[1;1Hα one\x1b[2;1H界 two\x1b[3;1H👍🏽 three\x1b[4;1H👩‍💻 four\x1b[5;1Hfive\x1b[6;1Hsix\x1b[7;1Hseven\x1b[8;1Height\x1b[9;1Hnine\x1b[10;1Hten\x1b[11;1Heleven".as_bytes()).screen(false);
                screen.cursor = (2, 4);
                screen.cursor_visible = true;
                surface.paint(&mut raster, &screen, &[]);
                surface.cache_handle(1);
                let mut retained = Vec::new();
                for (n, shift) in [1_isize, 3, -1, -4, 2, -2, 5, -5].into_iter().enumerate() {
                    retained.extend(surface.images(scale).into_iter().map(|(handle, _)| {
                        let pixels = handle.pixels().to_vec();
                        (handle, pixels)
                    }));
                    let count = shift.unsigned_abs() * screen.cols;
                    if shift > 0 {
                        screen.cells.rotate_left(count);
                    } else {
                        screen.cells.rotate_right(count);
                    }
                    let exposed = if shift > 0 {
                        screen.rows - shift as usize..screen.rows
                    } else {
                        0..shift.unsigned_abs()
                    };
                    for row in exposed {
                        for cell in &mut screen.cells[row * screen.cols..(row + 1) * screen.cols] {
                            cell.c = char::from(b'A' + n as u8);
                            cell.extra = 0;
                            cell.width = Default::default();
                            cell.bg = [n as u8 * 19, row as u8 * 13, 37];
                        }
                    }
                    screen.display_offset += 1;
                    screen.cursor = (n % screen.cols, n % screen.rows);
                    screen.cursor_visible = n % 3 != 0;
                    assert_eq!(
                        surface.scroll_shift(
                            &raster,
                            &screen,
                            &[true; 11],
                            surface.width,
                            surface.height,
                            11
                        ),
                        Some(shift)
                    );
                    assert!(!surface.paint(&mut raster, &screen, &[true; 11]).is_empty());
                    surface.cache_handle(n as u64 + 2);
                    assert_eq!(
                        surface.rgba(),
                        raster.render(&screen),
                        "scale={scale} shift={shift}"
                    );
                    for (handle, pixels) in &retained {
                        assert_eq!(handle.pixels().as_ref(), pixels);
                    }
                    assert!(surface.paint(&mut raster, &screen, &[false; 11]).is_empty());
                }
                // A new cluster table can reuse the same IDs for other text;
                // none of those old pixels are eligible for copying.
                screen.clusters = Default::default();
                screen.cells.rotate_left(screen.cols);
                assert_eq!(
                    surface.scroll_shift(
                        &raster,
                        &screen,
                        &[true; 11],
                        surface.width,
                        surface.height,
                        11
                    ),
                    None
                );
                surface.paint(&mut raster, &screen, &[true; 11]);
                assert_eq!(surface.rgba(), raster.render(&screen));
            }
        }
    }

    #[test]
    fn unicode_band_snapshots_retain_text_and_expand_spacer_damage() {
        let mut raster = Raster::for_test(1.25, 13.0, Cursor::Block).unwrap();
        let mut surface = Surface::default();
        let mut screen = term_core::terminal::Terminal::from_test_vt(
            8,
            8,
            "\x1b[4;2H👍🏽👍🏻\x1b[5;2H👩‍💻".as_bytes(),
        )
        .screen(false);
        screen.cursor_visible = false;
        surface.paint(&mut raster, &screen, &[]);
        surface.cache_handle(1);
        let retained = surface.images(1.25);
        let old = surface.rgba();
        let old_handles: Vec<_> = retained.iter().map(|(h, _)| h.pixels().to_vec()).collect();
        screen.cells[3 * 8 + 1].extra = screen.cells[3 * 8 + 3].extra;
        screen.cells[3 * 8 + 2].bg = [1, 51, 91];
        let mut dirty = [false; 8];
        dirty[3] = true;
        assert_eq!(
            surface.paint(&mut raster, &screen, &dirty),
            &[DamageBand {
                x: raster.width,
                y: 3 * raster.height,
                width: 2 * raster.width,
                height: raster.height,
            }]
        );
        surface.cache_handle(2);
        assert_ne!(surface.rgba(), old);
        assert_eq!(surface.rgba(), raster.render(&screen));
        for ((handle, _), bytes) in retained.iter().zip(old_handles) {
            assert_eq!(&handle.pixels()[..], bytes.as_slice());
        }
        screen.cursor = (2, 3);
        screen.cursor_visible = true;
        surface.paint(&mut raster, &screen, &[false; 8]);
        screen.cursor = (2, 4);
        surface.paint(&mut raster, &screen, &[false; 8]);
        assert_eq!(surface.rgba(), raster.render(&screen));
    }

    #[test]
    fn bands_preserve_pixels_ids_and_cursor_damage_across_boundaries() {
        let mut raster = Raster::for_test(2.5, 13.0, Cursor::Block).unwrap();
        let mut surface = Surface::default();
        let mut screen = Screen {
            clusters: Default::default(),
            cols: 13,
            rows: 11,
            cursor: (2, 3),
            cursor_visible: true,
            display_offset: 0,
            cells: vec![
                Cell {
                    extra: 0,
                    width: Default::default(),
                    c: 'M',
                    fg: [200, 210, 220],
                    bg: [10, 20, 30],
                    bold: false
                };
                143
            ],
            updated: Instant::now(),
        };
        surface.paint(&mut raster, &screen, &[]);
        surface.cache_handle(1);
        let retained = surface.images(2.5);
        let before = surface.rgba();
        screen.cursor.1 = 4;
        surface.paint(&mut raster, &screen, &[false; 11]);
        surface.cache_handle(2);
        let next = surface.images(2.5);
        assert_ne!(next[0].0.generation(), retained[0].0.generation());
        assert_ne!(next[1].0.generation(), retained[1].0.generation());
        assert_eq!(next[2].0.generation(), retained[2].0.generation());
        let old: Vec<u8> = retained
            .iter()
            .flat_map(|(handle, _)| {
                handle
                    .pixels()
                    .chunks_exact(4)
                    .flat_map(|p| [p[2], p[1], p[0], p[3]])
            })
            .collect();
        assert_eq!(before, old, "retained image pixels were mutated");
        assert_eq!(surface.rgba(), raster.render(&screen));

        // Several paints before handle refresh must not lose earlier changes.
        screen.cursor_visible = false;
        for row in [0, 7, 10] {
            screen.cells[row * 13].bg = [80, 90, 100];
            let mut dirty = [false; 11];
            dirty[row] = true;
            surface.paint(&mut raster, &screen, &dirty);
        }
        surface.cache_handle(3);
        assert_eq!(surface.rgba(), raster.render(&screen));
        let stable = surface.images(2.5);
        assert!(surface.paint(&mut raster, &screen, &[false; 11]).is_empty());
        surface.cache_handle(4);
        assert_eq!(
            stable.iter().map(|p| p.0.generation()).collect::<Vec<_>>(),
            surface
                .images(2.5)
                .iter()
                .map(|p| p.0.generation())
                .collect::<Vec<_>>()
        );

        // Malformed snapshots, shrink, column changes and invalidation all
        // owe complete valid pixels, including the final partial band.
        screen.cells.truncate(13 * 6);
        surface.paint(&mut raster, &screen, &[false; 11]);
        assert_eq!(surface.rgba(), raster.render(&screen));
        screen.cols = 6;
        surface.paint(&mut raster, &screen, &[false; 11]);
        assert_eq!(surface.rgba(), raster.render(&screen));
        surface.invalidate();
        surface.paint(&mut raster, &screen, &[false; 11]);
        assert_eq!(surface.rgba(), raster.render(&screen));
        screen.cols = 0;
        surface.paint(&mut raster, &screen, &[]);
        surface.cache_handle(5);
        assert!(surface.is_empty());
        assert!(surface.images(2.5).is_empty());
        screen.rows = 0;
        surface.paint(&mut raster, &screen, &[]);
        surface.cache_handle(5);
        assert!(surface.images(2.5).is_empty());
    }
}
