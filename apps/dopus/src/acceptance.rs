// SPDX-License-Identifier: MIT OR Apache-2.0
//! Optional shared acceptance fixture on the real Dopus window.
use application::{
    acceptance,
    iced::{Task, widget},
    inspect,
};

pub const ROOT_ID: &str = "dopus.fixture.root";
pub const VIEWPORT_ID: &str = "dopus.fixture.viewport";
pub const POINTS: &[&str] = &["dopus.prepare"];
static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn enabled() -> bool {
    ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn entry_id(index: usize) -> widget::Id {
    widget::Id::from(format!("dopus.fixture.left.entry.{index}"))
}
pub(crate) fn tag(
    element: application::Element<'static, crate::app::Msg>,
    alias: String,
) -> application::Element<'static, crate::app::Msg> {
    if enabled() {
        application::iced::widget::container(element)
            .id(widget::Id::from(format!("dopus.fixture.{alias}")))
            .into()
    } else {
        element
    }
}

pub(crate) fn setup() -> Result<(Option<acceptance::Fixture>, Task<crate::app::Msg>), String> {
    let Some(launch) = acceptance::Launch::from_env()? else {
        return Ok((None, Task::none()));
    };
    let mut targets = vec![
        inspect::Target::new("root", widget::Id::from(ROOT_ID)),
        inspect::Target::new("viewport", widget::Id::from(VIEWPORT_ID)),
        inspect::Target::new(
            "left-location",
            widget::Id::from(crate::view::location::LOCATION_LEFT),
        )
        .kind(inspect::Kind::TextInput),
        inspect::Target::new(
            "right-location",
            widget::Id::from(crate::view::location::LOCATION_RIGHT),
        )
        .kind(inspect::Kind::TextInput),
    ];
    targets.extend(
        (0..8).map(|index| inspect::Target::new(format!("left-entry-{index}"), entry_id(index))),
    );
    targets.push(inspect::Target::new(
        "left-chevron-0",
        widget::Id::from("dopus.fixture.left-chevron-0"),
    ));
    targets.extend((0..14).map(|index| {
        let alias = format!("toolbar-{index}");
        inspect::Target::new(
            alias.clone(),
            widget::Id::from(format!("dopus.fixture.{alias}")),
        )
    }));
    targets.extend(
        [
            "Home",
            "Filesystem",
            "Desktop",
            "Documents",
            "Downloads",
            "Music",
            "Pictures",
            "Videos",
        ]
        .into_iter()
        .map(|name| {
            let alias = format!("place-{name}");
            inspect::Target::new(
                alias.clone(),
                widget::Id::from(format!("dopus.fixture.{alias}")),
            )
        }),
    );
    let (fixture, task) =
        acceptance::Fixture::new(launch, POINTS, targets, inspect::Limits::new().aliases(64))?;
    ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok((Some(fixture), task))
}
