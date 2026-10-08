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

pub(crate) fn setup() -> Result<(Option<acceptance::Fixture>, Task<crate::app::Msg>), String> {
    let Some(launch) = acceptance::Launch::from_env()? else {
        return Ok((None, Task::none()));
    };
    let (fixture, task) = acceptance::Fixture::new(
        launch,
        POINTS,
        vec![
            inspect::Target::new("root", widget::Id::from(ROOT_ID)),
            inspect::Target::new("viewport", widget::Id::from(VIEWPORT_ID)),
            inspect::Target::new(
                "left-location",
                widget::Id::from(crate::view::location::LOCATION_LEFT),
            ),
            inspect::Target::new(
                "right-location",
                widget::Id::from(crate::view::location::LOCATION_RIGHT),
            ),
        ],
        inspect::Limits::new(),
    )?;
    Ok((Some(fixture), task))
}
