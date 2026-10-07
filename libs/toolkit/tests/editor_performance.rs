// SPDX-License-Identifier: MIT OR Apache-2.0
//! Real EditorPane preparation and tiny-skia raster phases, without a window,
//! screenshots or per-frame target allocations. The document is public-safe
//! synthetic JSON; measurement uses the production edit::Text/view iterator.
//! Run optimised with --ignored --nocapture. Timings are informational.

use std::cell::Cell;
use std::ops::Range;
use std::time::Instant;

use edit::{text::Text, view as measure};
use iced_core::text::Paragraph as _;
use iced_core::{
    Element, Event, Font, Pixels, Point, Rectangle, Size, mouse, renderer, text, window,
};
use iced_graphics::{Text as RenderText, Viewport};
use iced_runtime::{UserInterface, user_interface};
use iced_tiny_skia::Renderer;
use toolkit::EditorPane;
use toolkit::editor_pane::{self as pane};

#[derive(Default)]
struct Counters {
    walks: Cell<usize>,
    clusters: Cell<usize>,
    reads: Cell<usize>,
    read_bytes: Cell<usize>,
    checkpoints: Cell<usize>,
}

impl Counters {
    fn take(&self) -> [usize; 5] {
        [
            self.walks.replace(0),
            self.clusters.replace(0),
            self.reads.replace(0),
            self.read_bytes.replace(0),
            self.checkpoints.replace(0),
        ]
    }
}

struct Fixture {
    text: Text,
    spans: Vec<Vec<(Range<usize>, pane::Class)>>,
    counts: Counters,
}

impl Fixture {
    fn new() -> Self {
        let mut body = String::new();
        let mut spans = Vec::new();
        for line in 0..44 {
            let length: usize = if line == 20 {
                795
            } else {
                80 + line * 17 % 154
            };
            let mut row = format!(
                "  \"row_{line:02}\": {{\"label\":\"Synthetic scene\",\"x\":12,\"y\":34,\"enabled\":true,"
            );
            row.push_str(&"\"item\":123,".repeat(length.div_ceil(11)));
            row.truncate(length - 2);
            row.push_str("},");
            assert_eq!(row.len(), length);
            let base = body.len();
            // Precompute deliberately fragmented JSON-like token colours.
            // Lexing is outside these timers: this isolates widget/render cost.
            let mut tokens = Vec::new();
            let bytes = row.as_bytes();
            let mut at = 0;
            while at < bytes.len() {
                let first = at;
                let class = if bytes[at] == b'"' {
                    at += 1;
                    while at < bytes.len() && bytes[at] != b'"' {
                        at += 1;
                    }
                    at = (at + 1).min(bytes.len());
                    pane::Class::String
                } else if bytes[at].is_ascii_digit() {
                    while at < bytes.len() && bytes[at].is_ascii_digit() {
                        at += 1;
                    }
                    pane::Class::Number
                } else if bytes[at].is_ascii_punctuation() {
                    at += 1;
                    pane::Class::Punctuation
                } else {
                    at += 1;
                    pane::Class::Plain
                };
                tokens.push((base + first..base + at, class));
            }
            spans.push(tokens);
            body.push_str(&row);
            if line != 43 {
                body.push('\n');
            }
        }
        assert_eq!(body.lines().count(), 44);
        assert_eq!(body.lines().map(str::len).max(), Some(795));
        Self {
            text: Text::from_text(&body).unwrap(),
            spans,
            counts: Counters::default(),
        }
    }
}

struct Document<'a> {
    fixture: &'a Fixture,
    state: pane::ViewState,
    syntax: bool,
}

fn config(cfg: &pane::MeasureCfg) -> measure::MeasureCfg {
    measure::MeasureCfg {
        tab_size: cfg.tab_size,
        ambiguous_wide: cfg.ambiguous_wide,
    }
}

impl pane::Source for Document<'_> {
    fn identity(&self) -> u64 {
        1
    }
    fn revision(&self) -> u64 {
        0
    }
    fn len(&self) -> usize {
        self.fixture.text.len()
    }
    fn line_count(&self) -> usize {
        self.fixture.text.line_count()
    }
    fn line_start(&self, line: usize) -> Option<usize> {
        self.fixture.text.line_start(line)
    }
    fn line_range(&self, line: usize) -> Option<Range<usize>> {
        self.fixture.text.line_range(line)
    }
    fn content_end(&self, line: usize) -> usize {
        self.line_range(line).map_or(self.len(), |range| range.end)
    }
    fn line_of(&self, offset: usize) -> usize {
        let (mut first, mut last) = (1, self.line_count());
        while first < last {
            let mid = (first + last).div_ceil(2);
            if self.line_start(mid).is_some_and(|start| start <= offset) {
                first = mid;
            } else {
                last = mid - 1;
            }
        }
        first
    }
    fn clamp_offset(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.len());
        while offset > 0 && !self.fixture.text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }
    fn read(&self, range: Range<usize>, output: &mut String) {
        let counts = &self.fixture.counts;
        counts.reads.set(counts.reads.get() + 1);
        counts.read_bytes.set(counts.read_bytes.get() + range.len());
        self.fixture.text.read(range, output);
    }
    fn clusters(
        &self,
        cfg: &pane::MeasureCfg,
        range: Range<usize>,
        cells: usize,
    ) -> Box<dyn Iterator<Item = pane::Cluster> + '_> {
        let counts = &self.fixture.counts;
        counts.walks.set(counts.walks.get() + 1);
        Box::new(
            measure::clusters(&self.fixture.text, &config(cfg), range, cells).map(move |cluster| {
                counts.clusters.set(counts.clusters.get() + 1);
                pane::Cluster {
                    range: cluster.range,
                    cells: cluster.cells,
                    is_tab: cluster.is_tab,
                    ascii: cluster.ascii,
                }
            }),
        )
    }
    fn line_checkpoints(&self, cfg: &pane::MeasureCfg, line: usize) -> Vec<(usize, usize)> {
        let counts = &self.fixture.counts;
        counts.checkpoints.set(counts.checkpoints.get() + 1);
        measure::line_checkpoints(&self.fixture.text, &config(cfg), line)
    }
    fn state(&self) -> pane::ViewState {
        self.state.clone()
    }
    fn highlight_spans(
        &self,
        line: usize,
        _: &mut pane::SliceBudget,
    ) -> Vec<(Range<usize>, pane::Class)> {
        if self.syntax {
            self.fixture.spans[line - 1].clone()
        } else {
            Vec::new()
        }
    }
}

fn text_counts(renderer: &mut Renderer) -> (usize, usize, usize) {
    let (mut runs, mut bytes, mut quads) = (0, 0, 0);
    for layer in renderer.layers() {
        quads += layer.quads.len();
        for item in &layer.text {
            for text in item.as_slice() {
                if let RenderText::Cached { content, .. } = text {
                    runs += 1;
                    bytes += content.len();
                }
            }
        }
    }
    (runs, bytes, quads)
}

fn union(a: Rectangle, b: Rectangle) -> Rectangle {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rectangle {
        x,
        y,
        width: (a.x + a.width).max(b.x + b.width) - x,
        height: (a.y + a.height).max(b.y + b.height) - y,
    }
}

fn milliseconds(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// Keep failures useful even for a twenty-megabyte target. Count differing
/// pixels, retain one RGBA example and locate the affected rectangle.
fn pixel_difference(
    actual: &tiny_skia::Pixmap,
    expected: &tiny_skia::Pixmap,
    last_damage: Rectangle,
    scale: f32,
) -> Option<String> {
    assert_eq!(
        (actual.width(), actual.height()),
        (expected.width(), expected.height())
    );
    let width = actual.width() as usize;
    let physical = last_damage * scale;
    let mut count = 0;
    let mut outside = 0;
    let mut first = None;
    let (mut left, mut top, mut right, mut bottom) = (usize::MAX, usize::MAX, 0, 0);
    for (index, (a, b)) in actual
        .data()
        .chunks_exact(4)
        .zip(expected.data().chunks_exact(4))
        .enumerate()
    {
        if a == b {
            continue;
        }
        let (x, y) = (index % width, index / width);
        count += 1;
        outside += usize::from(
            (x as f32) < physical.x.floor()
                || (y as f32) < physical.y.floor()
                || (x as f32) >= (physical.x + physical.width).ceil()
                || (y as f32) >= (physical.y + physical.height).ceil(),
        );
        left = left.min(x);
        top = top.min(y);
        right = right.max(x + 1);
        bottom = bottom.max(y + 1);
        first.get_or_insert_with(|| format!("({x},{y}) actual={a:?} expected={b:?}"));
    }
    first.map(|first| {
        format!(
            "pixels={count} bbox=({left},{top})..({right},{bottom}) first={first} outside_last_damage={outside} last_damage_physical={physical:?}"
        )
    })
}

/// The pinned tiny-skia version strength-reduces an unmasked opaque fill to
/// Source, but keeps a masked fill in SourceOver. Fractional AA edges can
/// therefore differ by one RGB value. Keep that separate from the retained
/// editor oracle, which uses integer physical row boundaries.
fn opaque_aa_rounding_reproducer() {
    let path =
        tiny_skia::PathBuilder::from_rect(tiny_skia::Rect::from_xywh(0.0, 4.5, 12.0, 4.0).unwrap());
    let mut mask = tiny_skia::Mask::new(16, 12).unwrap();
    mask.fill_path(
        &tiny_skia::PathBuilder::from_rect(tiny_skia::Rect::from_xywh(2.0, 3.0, 4.0, 7.0).unwrap()),
        tiny_skia::FillRule::EvenOdd,
        false,
        tiny_skia::Transform::identity(),
    );
    let mut examples = Vec::new();
    for alpha in [255, 128] {
        let mut full = tiny_skia::Pixmap::new(16, 12).unwrap();
        full.fill(tiny_skia::Color::from_rgba8(35, 29, 27, 255));
        let mut partial = full.clone();
        let paint = tiny_skia::Paint {
            shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(53, 43, 38, alpha)),
            anti_alias: true,
            ..Default::default()
        };
        for (target, clipping) in [(&mut full, None), (&mut partial, Some(&mask))] {
            target.fill_path(
                &path,
                &paint,
                tiny_skia::FillRule::EvenOdd,
                tiny_skia::Transform::identity(),
                clipping,
            );
        }
        let index = (4 * 16 + 3) * 4;
        let (a, b) = (
            &full.data()[index..index + 4],
            &partial.data()[index..index + 4],
        );
        examples.push(format!("alpha={alpha} full={a:?} partial={b:?}"));
        if alpha == 255 {
            assert_ne!(a, b, "pinned tiny-skia opaque AA rounding reproducer");
        } else {
            assert_eq!(a, b, "both translucent paths retain SourceOver");
        }
    }
    eprintln!(
        "tiny-skia fractional AA blend-path reproducer: {}",
        examples.join("; ")
    );
}

#[test]
#[ignore = "manual real EditorPane/tiny-skia performance measurement"]
fn editor_render_phases_benchmark() {
    use std::hint::black_box;

    opaque_aa_rounding_reproducer();
    let fixture = Fixture::new();
    let view = pane::View {
        px: 14.0,
        // Exactly 20 logical / 50 physical pixels per row isolates retained
        // redraw correctness from the dependency rounding reproducer above.
        line_height: 20.0 / 14.0,
        ..pane::View::default()
    };
    let mut palette = pane::Palette::from(toolkit::Tokens::default());
    palette.highlight[pane::Class::String as usize] = palette.agent;
    palette.highlight[pane::Class::Number as usize] = palette.warning;
    palette.highlight[pane::Class::Punctuation as usize] = palette.gutter_text;
    let paragraph = iced_graphics::text::Paragraph::with_text(text::Text {
        content: "0123456789",
        bounds: Size::INFINITE,
        size: Pixels(view.px),
        line_height: text::LineHeight::Absolute(Pixels(view.px * view.line_height)),
        font: Font::MONOSPACE,
        align_x: text::Alignment::Left,
        align_y: iced_core::alignment::Vertical::Top,
        shaping: text::Shaping::Basic,
        wrapping: text::Wrapping::None,
        ellipsis: text::Ellipsis::None,
        hint_factor: None,
    });
    let cell_w = paragraph.min_bounds().width / 10.0;
    let gutter = (5.5 * cell_w + 4.0).round();
    let size = Size::new(gutter + 120.25 * cell_w, 760.0);
    assert!(
        size.width <= 1100.0,
        "120 columns exceed the physical fixture; cell_w={cell_w}"
    );
    let viewport = Viewport::with_physical_size(
        Size::new(2750, 1900),
        renderer::Scale {
            window: 2.5,
            application: 1.0,
        },
    );
    let full = Rectangle::with_size(viewport.logical_size());
    let head = fixture.text.line_start(20).unwrap() + 4;
    eprintln!(
        "editor fixture: bytes={} lines=44 longest=795 columns=120 physical=2750x1900 scale=2.5 font_px={} cell_w={cell_w:.3}",
        fixture.text.len(),
        view.px
    );
    let mut pixel_failures = Vec::new();
    for syntax in [false, true] {
        for case in [
            "cold full",
            "warm full",
            "caret narrow",
            "caret full",
            "caret then scroll",
            "vertical scroll",
            "horizontal scroll",
        ] {
            let mut renderer = Renderer::new(renderer::Settings::default());
            let mut cache = user_interface::Cache::default();
            let mut pixels = tiny_skia::Pixmap::new(2750, 1900).unwrap();
            let mut mask = tiny_skia::Mask::new(2750, 1900).unwrap();
            let mut caret = Rectangle::new(Point::ORIGIN, Size::new(2.0, 20.0));
            let mut last_damage = full;
            let mut samples = Vec::new();
            let mut post_caret_scroll = Vec::new();
            let mut totals = [0_usize; 8];
            for frame in 0..60 {
                if case == "cold full" {
                    renderer = Renderer::new(renderer::Settings::default());
                    cache = user_interface::Cache::default();
                }
                let mut state = pane::ViewState::default();
                let at = head
                    + if case.starts_with("caret") {
                        frame % 2
                    } else {
                        0
                    };
                state.sel = pane::Selection {
                    anchor: at,
                    head: at,
                };
                state.scroll = pane::Scroll {
                    first_line: if case == "vertical scroll" {
                        1 + frame % 8
                    } else if case == "caret then scroll" {
                        1 + (frame / 8) % 4
                    } else {
                        1
                    },
                    x_cells: if case == "horizontal scroll" {
                        frame % 16
                    } else {
                        0
                    },
                };
                let element: Element<'_, pane::Message, (), Renderer> = EditorPane::new(
                    Document {
                        fixture: &fixture,
                        state,
                        syntax,
                    },
                    &palette,
                    &view,
                )
                .into();
                let started = Instant::now();
                let mut ui = UserInterface::build(element, size, cache, &mut renderer);
                let layout_ms = milliseconds(started);
                let mut bus = iced_core::shell::Bus::new();
                let started = Instant::now();
                ui.update(
                    &window::Headless,
                    &iced_core::shell::Waker::noop(),
                    &[Event::Window(window::Event::RedrawRequested(
                        iced_core::time::Instant::now(),
                    ))],
                    mouse::Cursor::Unavailable,
                    &mut renderer,
                    &mut bus,
                );
                let update_ms = milliseconds(started);
                let previous_caret = caret;
                for message in bus.drain() {
                    if let pane::Message::Layout(report) = message {
                        assert!((report.editor[2] - report.gutter_w) / report.cell_w >= 120.0);
                        caret = Rectangle {
                            x: report.caret[0],
                            y: report.caret[1],
                            width: report.caret[2],
                            height: report.caret[3],
                        };
                    }
                }
                let started = Instant::now();
                ui.draw(
                    &mut renderer,
                    &(),
                    &renderer::Style {
                        text_color: palette.text,
                    },
                    mouse::Cursor::Unavailable,
                );
                let prepare_ms = milliseconds(started);
                cache = ui.into_cache();
                let counts = fixture.counts.take();
                let (runs, bytes, quads) = text_counts(&mut renderer);
                let damage = if frame > 0
                    && (case == "caret narrow" || (case == "caret then scroll" && frame % 8 != 0))
                {
                    union(previous_caret, caret).expand(1.0)
                } else {
                    full
                };
                let started = Instant::now();
                last_damage = damage;
                renderer.draw(
                    &mut pixels.as_mut(),
                    &mut mask,
                    &viewport,
                    &[damage],
                    palette.background,
                );
                let raster_ms = milliseconds(started);
                black_box(pixels.data());
                if frame >= 10 {
                    if case == "caret then scroll" && frame % 8 == 0 {
                        post_caret_scroll.push(raster_ms);
                    }
                    samples.push([layout_ms, update_ms, prepare_ms, raster_ms]);
                    for (total, value) in totals
                        .iter_mut()
                        .zip(counts.into_iter().chain([runs, bytes, quads]))
                    {
                        *total += value;
                    }
                }
            }
            // Narrow damage must preserve the complete last frame. This also
            // catches fixture geometry/caret mistakes without timing readback.
            let mut expected = tiny_skia::Pixmap::new(2750, 1900).unwrap();
            renderer.draw(
                &mut expected.as_mut(),
                &mut mask,
                &viewport,
                &[full],
                palette.background,
            );
            let difference =
                pixel_difference(&pixels, &expected, last_damage, viewport.scale_factor());
            let means: [f64; 4] = std::array::from_fn(|phase| {
                samples.iter().map(|s| s[phase]).sum::<f64>() / samples.len() as f64
            });
            let mut elapsed: Vec<_> = samples.iter().map(|s| s.iter().sum::<f64>()).collect();
            elapsed.sort_by(f64::total_cmp);
            if !post_caret_scroll.is_empty() {
                eprintln!(
                    "editor syntax={syntax} full scroll after seven narrow caret frames: raster_mean={:.3} ms",
                    post_caret_scroll.iter().sum::<f64>() / post_caret_scroll.len() as f64
                );
            }
            eprintln!(
                "editor syntax={syntax} {case}: mean layout={:.3} update={:.3} prepare={:.3} raster={:.3} total={:.3} p50={:.3} p99={:.3} ms; per-frame walks={} clusters={} reads={} read_bytes={} checkpoints={} text_runs={} text_bytes={} quads={}",
                means[0],
                means[1],
                means[2],
                means[3],
                means.iter().sum::<f64>(),
                elapsed[25],
                elapsed[49],
                totals[0] / 50,
                totals[1] / 50,
                totals[2] / 50,
                totals[3] / 50,
                totals[4] / 50,
                totals[5] / 50,
                totals[6] / 50,
                totals[7] / 50
            );
            if let Some(difference) = difference {
                let failure =
                    format!("case={case} syntax={syntax}: {difference}; caret_logical={caret:?}");
                eprintln!("editor pixel oracle FAILED: {failure}");
                pixel_failures.push(failure);
            }
        }
    }
    assert!(
        pixel_failures.is_empty(),
        "editor pixel oracle mismatches:\n{}",
        pixel_failures.join("\n")
    );
}
