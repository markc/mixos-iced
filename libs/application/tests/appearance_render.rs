// SPDX-License-Identifier: MIT OR Apache-2.0
//! Real offscreen tiny-skia frames through shared typography and theme defaults.
//! This is an adapter renderer fixture, not a Ced/Quoin or native-VT session.
#![cfg(all(feature = "settings", feature = "tiny-skia", feature = "test-support"))]
use application::{
    iced::{
        Element, Size,
        widget::{button, column, container},
    },
    presentation::Host,
    test::Simulator,
};
use settings::{Binding, Desktop, Revision, Snapshot, consumer::Consumer};
use std::path::{Path, PathBuf};
use toolkit::fonts::{self, FontChoice, FontSet};

#[derive(Clone, Debug)]
enum Message {
    Button,
    Input(String),
}
fn snapshot(rev: u64, dark: bool) -> Snapshot {
    let mut desktop = Desktop::default();
    if dark {
        desktop.appearance.mode = "dark".into();
        desktop.ui.text_scale = 1.5;
        desktop.ui.density = 0.5;
    }
    Snapshot {
        schema: 1,
        binding: Binding {
            instance: "fixture".into(),
            profile: "default".into(),
        },
        incarnation: "render".into(),
        revision: Revision(rev),
        design_revision: Revision(rev),
        source_digest: settings::source_digest(settings::EMBEDDED_DEFAULT_SOURCE),
        effective: settings::resolve(&desktop).unwrap(),
        desktop,
    }
}
fn host(shell: bool) -> Host<String> {
    let binding = snapshot(1, false).binding;
    let mut c = if shell {
        Consumer::for_shell(binding).unwrap()
    } else {
        Consumer::for_app(binding, "ced").unwrap()
    };
    let subscribe = c.connected(1).unwrap();
    let read = c.complete(&subscribe, Ok(None)).unwrap();
    c.complete(&read, Ok(Some(snapshot(1, false))));
    Host::new(c)
}
fn activate(host: &mut Host<String>) {
    let ready = host.request().unwrap().prepare(
        |name, record| {
            // The fixture pins Inter for all records to avoid workstation fonts.
            // Production preparation uses each actual declared chain/role instead.
            fonts::try_font_for("Inter", &[], record.weight, false, None, false)
                .map_err(|message| settings::Diagnostic::new("font_unavailable", name, message))
        },
        |_| Ok("document stays here".to_owned()),
    );
    host.complete(ready).unwrap();
}
fn render(host: &Host<String>, name: &str) -> Vec<u8> {
    let presentation = host.presentation().unwrap();
    let look = presentation.appearance();
    let ui = look.typography().get("ui").unwrap();
    let t = look.tokens();
    let element: Element<'_, Message, toolkit::Theme, application::cpu::Renderer> = container(
        column![
            ui.text("Shared settings defaults"),
            button(ui.text("Action"))
                .padding(t.metrics.spacing.md)
                .on_press(Message::Button),
            ui.input("Document", presentation.content())
                .on_input(Message::Input),
        ]
        .spacing(t.metrics.spacing.md),
    )
    .padding(t.metrics.spacing.lg)
    .into();
    let mut simulator = Simulator::with_size(Default::default(), Size::new(480.0, 240.0), element);
    let image = simulator.snapshot(&look.theme()).unwrap();
    let directory = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("settings-appearance-{}", std::process::id()));
    let path = directory.join(format!("{name}.png"));
    assert!(image.matches_image(&path).unwrap());
    let written = directory.join(format!("{name}-tiny-skia.png"));
    eprintln!("settings appearance frame: {}", written.display());
    read_png(&written, t.palette.surface)
}
fn read_png(path: &Path, background: application::iced::Color) -> Vec<u8> {
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut rgba = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut rgba).unwrap();
    rgba.truncate(info.buffer_size());
    assert_eq!((info.width, info.height), (960, 480));
    let expected = background.into_rgba8();
    let corner = &rgba[..4];
    assert!(corner.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 2));
    // More than a background clear: text and controls actually painted pixels.
    assert!(rgba.chunks_exact(4).filter(|p| *p != corner).count() > 1000);
    rgba
}
#[test]
fn shared_app_and_shell_defaults_render_and_live_swap_preserves_content() {
    fonts::install(
        FontSet::new().sans(
            include_bytes!("../../../vendor/font/Inter-VariableFont_opsz,wght.ttf").as_slice(),
        ),
        None,
    )
    .unwrap();
    assert!(fonts::try_font_for("missing-fixture-font", &[], 400, false, None, false).is_err());
    let generic = fonts::try_font_for("missing-fixture-font", &[], 400, false, None, true).unwrap();
    assert_eq!(generic.choice, FontChoice::Generic);
    let mut app = host(false);
    let mut shell = host(true);
    activate(&mut app);
    activate(&mut shell);
    let initial = render(&app, "app-light");
    assert_eq!(initial, render(&shell, "shell-light"));
    app.consumer_mut().observe(1, snapshot(2, true));
    shell.consumer_mut().observe(1, snapshot(2, true));
    activate(&mut app);
    activate(&mut shell);
    let changed = render(&app, "app-dark-scaled");
    assert_ne!(initial, changed);
    assert_eq!(changed, render(&shell, "shell-dark-scaled"));
    assert_eq!(app.presentation().unwrap().content(), "document stays here");
    assert_eq!(app.consumer().applied().unwrap().revision, Revision(2));
    // Exercise the shared input constructor's message type without discarding
    // its payload as dead code; it belongs to the application model.
    let Message::Input(value) = Message::Input(app.presentation().unwrap().content().clone())
    else {
        unreachable!()
    };
    assert_eq!(value, "document stays here");
}
