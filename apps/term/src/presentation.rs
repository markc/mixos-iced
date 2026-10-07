// SPDX-License-Identifier: MIT OR Apache-2.0
//! Immutable terminal painter preparation on the shared settings worker.
use term_core::{
    config::Cursor,
    font::FontSize,
    raster::{OwnedFace, OwnedFontPolicy, PreparedRaster},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LocalContext {
    pub scale: f32,
    pub zoom_steps: i32,
    pub cursor: Cursor,
}
impl LocalContext {
    pub fn new(scale: f32, cursor: Cursor) -> Result<Self, String> {
        if !scale.is_finite() || !(0.5..=8.0).contains(&scale) {
            return Err("terminal scale must be finite and in 0.5..8".into());
        }
        Ok(Self {
            scale,
            zoom_steps: 0,
            cursor,
        })
    }
    pub fn with_scale(self, scale: f32) -> Result<Self, String> {
        Ok(Self {
            zoom_steps: self.zoom_steps,
            ..Self::new(scale, self.cursor)?
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RasterSource {
    /// Ordered actual font identities, with no requested-family attribution.
    Verified(Vec<Vec<(String, u64, u32)>>),
    /// The already resolved immutable startup raster, with no verified binding.
    Bootstrap,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RasterKey {
    pub source: RasterSource,
    pub weight: u16,
    pub scale: u32,
    pub logical_px: u32,
    pub cursor: Cursor,
}

#[derive(Clone)]
pub(crate) struct Content {
    pub raster: PreparedRaster,
    pub key: RasterKey,
    pub font: FontSize,
    pub context: LocalContext,
}

pub(crate) fn prepare(
    appearance: &appearance::settings::Prepared,
    _snapshot: &settings::Snapshot,
    local: &LocalContext,
    bootstrap: &PreparedRaster,
) -> Result<Content, settings::Diagnostic> {
    let fault =
        |message| settings::Diagnostic::new("terminal_resources", "typography.terminal", message);
    LocalContext::new(local.scale, local.cursor).map_err(fault)?;
    let terminal = appearance
        .typography()
        .get("terminal")
        .ok_or_else(|| fault("Terminal typography is missing".into()))?;
    let font = FontSize::from_steps(terminal.size, local.zoom_steps).map_err(fault)?;
    let resources = appearance.resources();
    let verified = resources.and_then(|r| r.binding()).is_some();
    let (raster, source) = if verified {
        let owned = resources
            .and_then(|r| r.owned_text("terminal"))
            .ok_or_else(|| fault("Verified terminal font policy is missing".into()))?;
        let groups = owned
            .groups()
            .iter()
            .map(|group| {
                group
                    .iter()
                    .map(|face| OwnedFace {
                        bytes: face.bytes(),
                        index: face.index(),
                    })
                    .collect()
            })
            .collect();
        let source = RasterSource::Verified(
            owned
                .groups()
                .iter()
                .map(|group| {
                    group
                        .iter()
                        .map(|face| {
                            (
                                face.evidence().source.clone(),
                                face.bytes().len() as u64,
                                face.index(),
                            )
                        })
                        .collect()
                })
                .collect(),
        );
        let raster = PreparedRaster::prepare(
            OwnedFontPolicy {
                groups,
                weight: owned.effective_weight(),
            },
            local.scale,
            font.current(),
            local.cursor,
        )
        .map_err(fault)?;
        (raster, source)
    } else {
        (
            bootstrap
                .resized_with_cursor(local.scale, font.current(), local.cursor)
                .map_err(fault)?,
            RasterSource::Bootstrap,
        )
    };
    let key = RasterKey {
        source,
        weight: raster.weight(),
        scale: local.scale.to_bits(),
        logical_px: font.current().to_bits(),
        cursor: local.cursor,
    };
    Ok(Content {
        raster,
        key,
        font,
        context: *local,
    })
}

/// Complete grid and physical extent last delivered to the real PTY lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PtyExtent(pub u16, pub u16, pub u16, pub u16);
impl PtyExtent {
    pub fn new(grid: (u16, u16), cell: (u32, u32)) -> Self {
        let pixels = |count: u16, size: u32| {
            // winsize exposes u16 pixel fields. Saturate only this OS report;
            // the painter and terminal grid retain their complete geometry.
            u16::try_from(u64::from(count) * u64::from(size)).unwrap_or(u16::MAX)
        };
        Self(
            grid.0,
            grid.1,
            pixels(grid.0, cell.0),
            pixels(grid.1, cell.1),
        )
    }
    pub fn grid(self) -> (u16, u16) {
        (self.0, self.1)
    }
}

pub(crate) struct LayoutTarget<'a> {
    pub painter: &'a crate::frame::Painter,
    pub tabs: &'a std::sync::Arc<std::sync::Mutex<term_core::tabs::TabSet>>,
    pub shape: &'a mut crate::layout::Shape,
    pub window: application::iced::Size,
    pub chrome: f32,
    pub grids: &'a mut std::collections::HashMap<u64, PtyExtent>,
    pub paint_requested: &'a mut bool,
}
impl LayoutTarget<'_> {
    pub fn relayout(&mut self) {
        let scale = self.painter.scale();
        let cell = self.painter.cell();
        if cell.0 == 0 || cell.1 == 0 {
            return;
        }
        let bounds = crate::layout::content(self.window.width, self.window.height, self.chrome);
        let mut resize = Vec::new();
        {
            let mut tabs = self.tabs.lock().expect("tabs");
            *self.shape = crate::layout::Shape::of(&tabs);
            if tabs.is_empty() {
                return;
            }
            let Some(tree) = &self.shape.tree else {
                return;
            };
            let placed = crate::layout::panes(tree, bounds, scale);
            for (id, geometry) in &placed {
                tabs.geometry(
                    *id,
                    term_core::panes::Geometry {
                        x: geometry.x - bounds.x,
                        y: geometry.y - bounds.y,
                        ..*geometry
                    },
                );
                let extent = PtyExtent::new(crate::layout::grid(*geometry, cell, scale), cell);
                if self.grids.get(id) != Some(&extent)
                    && let Some(terminal) = tabs.pane_by_id(*id)
                {
                    resize.push((*id, extent, terminal));
                }
            }
        }
        let visible = self.shape.visible();
        self.grids.retain(|id, _| visible.contains(id));
        // Drop the set lock before taking terminal locks. This updates the
        // model synchronously and enqueues the real asynchronous PTY ioctl.
        for (id, extent, terminal) in resize {
            *self.paint_requested = true;
            terminal
                .lock()
                .expect("terminal")
                .resize(extent.0, extent.1, extent.2, extent.3);
            self.tabs
                .lock()
                .expect("tabs")
                .resized(id, extent.0, extent.1);
            self.grids.insert(id, extent);
        }
    }
}

pub(crate) struct ActivationTarget<'a> {
    pub painter: &'a mut crate::frame::Painter,
    pub tokens: &'a mut toolkit::Tokens,
    pub ui: &'a mut toolkit::typography::TextStyle,
    pub chrome: &'a mut f32,
    pub baseline: &'a mut f32,
    pub applied_context: &'a mut Option<LocalContext>,
    pub applied_key: &'a mut Option<RasterKey>,
    pub force_paint: &'a mut bool,
    pub paint_requested: &'a mut bool,
    pub tabs: &'a std::sync::Arc<std::sync::Mutex<term_core::tabs::TabSet>>,
    pub shape: &'a mut crate::layout::Shape,
    pub window: application::iced::Size,
    pub grids: &'a mut std::collections::HashMap<u64, PtyExtent>,
}
impl ActivationTarget<'_> {
    pub fn activate(&mut self, presentation: &application::presentation::Presentation<Content>) {
        let content = presentation.content();
        let raster_changed = self.applied_key.as_ref() != Some(&content.key);
        if raster_changed {
            self.painter.activate_prepared(content);
            *self.applied_key = Some(content.key.clone());
            *self.force_paint = true;
            *self.paint_requested = true;
        }
        self.painter.set_applied_font(content.font);
        *self.applied_context = Some(content.context);
        *self.baseline = content.font.configured();
        let prepared = presentation.appearance();
        *self.tokens = crate::theme::tokens(prepared);
        *self.ui = prepared
            .typography()
            .get("ui")
            .expect("prepared UI typography");
        let chrome = crate::layout::strip_height(self.painter.scale(), *self.ui);
        *self.chrome = chrome;
        // Model mutations may share this wake even when only colours changed.
        // Read the live tree under its lock before returning to the ACK seam.
        LayoutTarget {
            painter: self.painter,
            tabs: self.tabs,
            shape: self.shape,
            window: self.window,
            chrome,
            grids: self.grids,
            paint_requested: self.paint_requested,
        }
        .relayout();
        self.painter.retain(&self.shape.visible());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pty_extent_includes_pixel_only_changes_and_saturates_os_fields_without_overflow() {
        assert_ne!(
            PtyExtent::new((80, 24), (8, 16)),
            PtyExtent::new((80, 24), (9, 17))
        );
        assert_eq!(
            PtyExtent::new((80, 24), (8, 16)),
            PtyExtent(80, 24, 640, 384)
        );
        assert_eq!(
            PtyExtent::new((u16::MAX, u16::MAX), (512, 1024)),
            PtyExtent(u16::MAX, u16::MAX, u16::MAX, u16::MAX)
        );
    }
    #[test]
    fn local_context_rejects_non_finite_or_unsupported_scale_and_keeps_zoom() {
        let mut context = LocalContext::new(1.0, Cursor::Block).unwrap();
        context.zoom_steps = 7;
        assert_eq!(context.with_scale(2.5).unwrap().zoom_steps, 7);
        for scale in [f32::NAN, f32::INFINITY, 0.0, 8.1] {
            assert!(context.with_scale(scale).is_err());
        }
    }
}
