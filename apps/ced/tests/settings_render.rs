// SPDX-License-Identifier: MIT OR Apache-2.0
//! Ced's actual appearance adapter and chrome render through tiny-skia. This
//! offscreen fixture does not claim native window/first-map/session coverage.
use application::{iced::{Element, Size, widget::{column, container}}, presentation::Host, test::Simulator};
use ced::{chrome::Look, theme};
use settings::{Binding, Desktop, Revision, Snapshot, consumer::Consumer};
use toolkit::fonts::{FontChoice, FontSelection, FontSet};

#[derive(Clone, Debug)]
enum Message { Action }
fn snapshot(revision: u64, dark: bool) -> Snapshot {
    let mut desktop = Desktop::default();
    if dark {
        desktop.appearance.mode = "dark".into();
        desktop.ui.text_scale = 1.5;
        desktop.ui.density = 0.5;
    }
    Snapshot { schema: 1, binding: Binding { instance: "fixture".into(), profile: "default".into() }, incarnation: "ced-render".into(), revision: Revision(revision), design_revision: Revision(revision),
        source_digest: settings::source_digest(settings::EMBEDDED_DEFAULT_SOURCE), effective: settings::resolve(&desktop).unwrap(), desktop }
}
fn activate(host: &mut Host<theme::Theme>) {
    let request = host.request().unwrap();
    let snapshot = request.update().snapshot().clone();
    let ready = request.prepare(|_, record| {
        toolkit::fonts::try_font_for("Inter", &[], record.weight, false, None, false)
            .map_err(|message| settings::Diagnostic::new("font_unavailable", "fixture", message))
            .map(|selection| FontSelection { font: selection.font, choice: FontChoice::Declared })
    }, |look| theme::from_settings(look, &snapshot));
    assert!(host.complete(ready).is_some());
}
fn render(host: &Host<theme::Theme>, name: &str) -> Vec<u8> {
    let theme = host.presentation().unwrap().content();
    let look = Look::new(theme, theme.mono.1);
    let element: Element<'_, Message> = container(column![
        look.text("Ced settings chrome"),
        look.code("Buffer content remains in the editor model"),
        look.button("Action", Some(Message::Action)).style(look.flat()),
    ].spacing(look.tokens.metrics.spacing.md)).padding(look.tokens.metrics.spacing.lg)
        .style(look.strip(theme.palette.background, theme.palette.text)).into();
    let mut simulator = Simulator::with_size(Default::default(), Size::new(480.0, 240.0), element);
    let image = simulator.snapshot(&theme.iced_theme()).unwrap();
    let directory = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("ced-settings-{}", std::process::id()));
    let path = directory.join(format!("{name}.png"));
    assert!(image.matches_image(&path).unwrap());
    let path = directory.join(format!("{name}-tiny-skia.png"));
    eprintln!("Ced settings chrome frame: {}", path.display());
    std::fs::read(path).unwrap()
}
#[test]
fn ced_chrome_uses_current_palette_tokens_and_scaled_fonts() {
    toolkit::fonts::install(FontSet::new().sans(include_bytes!("../../../vendor/font/Inter-VariableFont_opsz,wght.ttf").as_slice()), None).unwrap();
    let first = snapshot(1, false);
    let mut consumer = Consumer::for_app(first.binding.clone(), "ced").unwrap();
    let subscribe = consumer.connected(1).unwrap();
    let read = consumer.complete(&subscribe, Ok(None)).unwrap();
    consumer.complete(&read, Ok(Some(first)));
    let mut host = Host::new(consumer);
    activate(&mut host);
    let initial = render(&host, "light");
    let ui = host.presentation().unwrap().content().ui.1;
    let spacing = host.presentation().unwrap().content().tokens.metrics.spacing.md;
    host.consumer_mut().observe(1, snapshot(2, true));
    activate(&mut host);
    let theme = host.presentation().unwrap().content();
    assert_eq!(theme.ui.1, ui * 1.5);
    assert_eq!(theme.tokens.metrics.spacing.md, spacing * 0.5);
    assert_ne!(initial, render(&host, "dark-scaled"));
    assert_eq!(host.consumer().applied().unwrap().revision, Revision(2));
    host.consumer_mut().observe(1, snapshot(3, true));
    assert!(host.request().is_none());
    assert_eq!(host.consumer().applied().unwrap().revision, Revision(3));
}
