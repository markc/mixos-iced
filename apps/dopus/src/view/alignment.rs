// SPDX-License-Identifier: MIT OR Apache-2.0
//! First content baseline across Places, pane locations and Properties.
use super::{Look, elide, location};
use application::iced::advanced::text::Paragraph as _;

#[derive(Debug, Clone, Copy)]
pub struct FirstRow {
    pub places_top: f32,
    pub pane_top: f32,
    pub properties_top: f32,
}

fn baseline(paragraph: &application::iced::advanced::graphics::text::Paragraph) -> f32 {
    paragraph
        .buffer()
        .layout_runs()
        .next()
        .map_or(0.0, |line| line.line_y)
}

impl FirstRow {
    pub fn new(look: Look) -> Self {
        let sidebar = elide::shape_with_line_height(
            "Ag",
            look.ui_font,
            look.sidebar_px(),
            look.sidebar_line_height(),
        );
        let location = elide::shape_with_line_height(
            "Ag",
            look.mono_font,
            location::text_px(look),
            look.mono_line_height
                .map(|height| height * location::text_px(look) / look.mono_px),
        );
        let sidebar_baseline = baseline(&sidebar);
        let location_baseline = baseline(&location);
        // Home's label is centred beside an icon inside a padded button.
        let icon_offset = (look.chrome.icon - sidebar.min_bounds().height).max(0.0) / 2.0;
        let places_inner = look.chrome.small + icon_offset + sidebar_baseline;
        let pane_inner = location::padding(look).top + location_baseline;
        // Keep the location's existing inset unless the icon/font requires
        // more room. Every derived outer inset remains non-negative.
        let baseline = (look.chrome.small + pane_inner)
            .max(places_inner)
            .max(sidebar_baseline);
        Self {
            places_top: baseline - places_inner,
            pane_top: baseline - pane_inner,
            properties_top: baseline - sidebar_baseline,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::Msg, icons::Icons, theme};
    use application::Element;
    use application::cpu::Renderer;
    use application::iced::Size;
    use application::iced::advanced::{Layout, graphics::text::Paragraph, layout, widget::Tree};
    use design::{Mode, Scheme};
    use dopus_core::{DOpusConfig, DopusCore, PaneId, properties::Properties};

    fn look() -> Look {
        let theme = theme::resolve_selection(
            &theme::Selection {
                scheme: Scheme::Ocean,
                mode: Mode::Light,
                design_source: None,
            },
            Vec::new(),
        );
        assert!((theme.sidebar_px - 13.2).abs() < 0.0001);
        Look {
            sidebar_px: theme.sidebar_px,
            small_px: theme.small_px,
            tokens: theme.tokens,
            chrome: theme.chrome,
            ui_font: theme.ui_font,
            mono_font: theme.mono_font,
            small_font: theme.small_font,
            px: theme.ui_px(),
            mono_px: theme.mono.1,
            density: theme.density,
            ui_line_height: theme.ui_line_height,
            mono_line_height: theme.mono_line_height,
            small_line_height: theme.small_line_height,
        }
    }

    fn layout(mut element: Element<'_, Msg>, look: Look, width: f32) -> (Tree, layout::Node) {
        let renderer = Renderer::new(application::iced::advanced::renderer::Settings {
            default_font: look.ui_font,
            default_text_size: look.px.into(),
            ..Default::default()
        });
        let mut tree = Tree::new(element.as_widget());
        element.as_widget_mut().diff(&mut tree);
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(width, 600.0)),
        );
        (tree, node)
    }

    fn first_label(tree: &Tree) -> Option<&Paragraph> {
        if tree.tag == application::iced::advanced::widget::tree::Tag::of::<Paragraph>() {
            Some(tree.state.downcast_ref::<Paragraph>())
        } else {
            tree.children.iter().find_map(first_label)
        }
    }

    fn label_bounds(node: &layout::Node, path: &[usize]) -> application::iced::Rectangle {
        let mut layout = Layout::new(node);
        for index in path {
            layout = layout.child(*index);
        }
        layout.bounds()
    }

    #[test]
    fn first_content_baselines_include_all_padding_icons_and_font_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = DOpusConfig::default();
        config.left.path = dir.path().to_owned();
        config.right.path = dir.path().to_owned();
        let (core, _rx) = DopusCore::new(config, None);
        let icons = Icons::new();
        let places = [("Home", dir.path().to_owned())];
        let base = look();
        // Also exercise a larger label and icon instead of baking in one
        // desktop's installed font metrics or the default icon/text ratio.
        let mut larger = base;
        larger.sidebar_px *= 1.5;
        larger.chrome.icon *= 2.0;
        for look in [base, larger] {
            let first_row = FirstRow::new(look);
            let elements = [
                super::super::places::sidebar(
                    look,
                    first_row,
                    &icons,
                    "",
                    PaneId::Left,
                    core.pane(PaneId::Left),
                    &places,
                    &[],
                ),
                super::super::panes::pane_header(
                    look,
                    first_row,
                    core.pane(PaneId::Left),
                    PaneId::Left,
                    true,
                    None,
                ),
                super::super::properties::sidebar(
                    look,
                    first_row,
                    Properties::Folder {
                        path: "Home".into(),
                        summary: String::new(),
                    },
                ),
            ];
            // Paths follow real layout nodes, including the transparent
            // tooltip wrapper (no node) and the row's icon before Home.
            let paths: [&[usize]; 3] = [&[0, 0, 0, 0, 1], &[0, 0, 0], &[0, 0, 0]];
            let mut offsets = Vec::new();
            for (element, path) in elements.into_iter().zip(paths) {
                let (tree, node) = layout(element, look, 320.0);
                let paragraph = first_label(&tree).expect("first content label");
                offsets.push(
                    label_bounds(&node, path).y
                        + paragraph.buffer().layout_runs().next().unwrap().line_y,
                );
            }
            for offset in &offsets[1..] {
                assert!((offset - offsets[0]).abs() < 0.0001, "{offsets:?}");
            }
        }
    }

    #[test]
    fn summary_footer_elides_inside_even_a_twenty_five_pixel_pane() {
        let look = look();
        let summary = "123456789 folders, 987654321 files (999.9 GiB)";
        let mut measurements = super::super::Measurements::default();
        for width in [0.0, 8.0, 25.0, 240.0, 800.0] {
            let (tree, node) = layout(
                super::super::panes::summary_footer(
                    look,
                    measurements.footer(look, PaneId::Left, summary.into()),
                ),
                look,
                width,
            );
            let bounds = label_bounds(&node, &[0]);
            let paragraph = first_label(&tree).unwrap();
            assert!(node.bounds().width <= width);
            assert!(paragraph.min_bounds().width <= bounds.width);
            assert!(bounds.x + bounds.width <= node.bounds().width);
            if width == 25.0 {
                assert!(
                    paragraph.min_bounds().width
                        < elide::shape(summary, look.ui_font, look.small_px)
                            .min_bounds()
                            .width
                );
            }
        }
    }

    #[test]
    fn measurements_reuse_unchanged_look_and_summary() {
        super::super::Measurements::assert_cache_invalidation(look());
    }

    #[test]
    fn missing_baseline_is_zero() {
        assert_eq!(baseline(&Paragraph::new()), 0.0);
    }
}
