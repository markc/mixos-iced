// SPDX-License-Identifier: MIT OR Apache-2.0
//! The portable compound example consumes all three panes through real iced.
#![cfg(all(feature = "gallery-tiny-skia", not(feature = "wgpu")))]
#[allow(dead_code)]
#[path = "../examples/compounds/app.rs"]
mod app;
use iced_test::Simulator;
use iced_test::core::{Settings, Size};
use toolkit::editor_pane::Source;

#[test]
fn portable_document_receives_unicode_input_through_the_editor_pane() {
    let mut app = app::Demo::new();
    let mut ui = Simulator::with_size(Settings::default(), Size::new(1100.0, 700.0), app.view());
    ui.typewrite("e\u{301}中");
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(messages.iter().any(|message| matches!(
        message,
        app::Message::Editor(toolkit::editor_pane::Message::Command(_))
    )));
    for message in messages {
        app.update(message);
    }
    let source = app.document();
    let mut body = String::new();
    source.read(0..source.len(), &mut body);
    assert!(body.starts_with("e\u{301}中"), "{body}");
}

#[test]
fn all_compound_surfaces_render_with_dark_and_light_tokens() {
    let mut app = app::Demo::new();
    let directory = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("toolkit-snapshots");
    std::fs::create_dir_all(&directory).unwrap();
    let mut frames = Vec::new();
    for (index, name) in ["dark", "light"].into_iter().enumerate() {
        if index != 0 {
            app.update(app::Message::Theme);
        }
        let mut ui =
            Simulator::with_size(Settings::default(), Size::new(1100.0, 700.0), app.view());
        ui.find("Synthetic files").expect("file pane navigation");
        ui.find("$ shared terminal surface")
            .expect("terminal surface");
        let path = directory.join(format!("compounds-{name}.png"));
        let written = directory.join(format!("compounds-{name}-tiny-skia.png"));
        let _ = std::fs::remove_file(&written);
        assert!(
            ui.snapshot(&app.theme())
                .unwrap()
                .matches_image(&path)
                .unwrap()
        );
        frames.push(std::fs::read(written).unwrap());
    }
    assert_ne!(frames[0], frames[1]);
}

#[test]
fn compound_surfaces_render_at_native_and_fractional_scale() {
    use iced_core::renderer::Headless;
    use iced_core::{mouse, renderer, theme::Base};
    let app = app::Demo::new();
    let size = Size::new(1100.0, 700.0);
    let mut renderer = iced_futures::futures::executor::block_on(<iced_renderer::Renderer as Headless>::new(
        renderer::Settings::default(),
        Some("tiny-skia"),
    ))
    .expect("software renderer");
    let theme = app.theme();
    let base = theme.base();
    let mut ui = iced_runtime::UserInterface::build(
        app.view(),
        size,
        iced_runtime::user_interface::Cache::default(),
        &mut renderer,
    );
    let directory = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("toolkit-snapshots");
    std::fs::create_dir_all(&directory).unwrap();
    for scale in [1.0, 2.5] {
        ui.draw(
            &mut renderer,
            &theme,
            &renderer::Style {
                text_color: base.text_color,
            },
            mouse::Cursor::Unavailable,
        );
        let physical = Size::new((size.width * scale) as u32, (size.height * scale) as u32);
        let pixels = renderer.screenshot(physical, scale, base.background_color);
        assert_eq!(
            pixels.len(),
            (physical.width * physical.height * 4) as usize
        );
        // Check painted content in each third, including the terminal footer.
        let background = &pixels[..4];
        for (start, end) in [
            (0, physical.width / 2),
            (physical.width / 2, physical.width),
        ] {
            let painted = (0..physical.height)
                .flat_map(|y| (start..end).map(move |x| (y * physical.width + x) as usize * 4))
                .filter(|offset| &pixels[*offset..*offset + 4] != background)
                .count();
            assert!(
                painted > (100.0 * scale * scale) as usize,
                "blank compound surface at {scale}"
            );
        }
        let file =
            std::fs::File::create(directory.join(format!("compounds-scale-{scale}.png"))).unwrap();
        let mut encoder = png::Encoder::new(file, physical.width, physical.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&pixels)
            .unwrap();
    }
}
