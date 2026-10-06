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
    assert!(messages.iter().any(|message| matches!(message, app::Message::Editor(toolkit::editor_pane::Message::Command(_)))));
    for message in messages { app.update(message); }
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
        if index != 0 { app.update(app::Message::Theme); }
        let mut ui = Simulator::with_size(Settings::default(), Size::new(1100.0, 700.0), app.view());
        ui.find("Synthetic files").expect("file pane navigation");
        ui.find("$ shared terminal surface").expect("terminal surface");
        let path = directory.join(format!("compounds-{name}.png"));
        assert!(ui.snapshot(&app.theme()).unwrap().matches_image(&path).unwrap());
        frames.push(std::fs::read(path).unwrap());
    }
    assert_ne!(frames[0], frames[1]);
}
