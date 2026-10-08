// SPDX-License-Identifier: MIT OR Apache-2.0
//! The shared fixture on BusViewer's existing GUI and Bus workers.
use application::{
    acceptance,
    iced::{Task, widget},
    inspect,
};

pub const ROOT_ID: &str = "busviewer.fixture.root";
pub const POINTS: &[&str] = &["busviewer.prepare"];

pub(crate) fn setup() -> Result<(Option<acceptance::Fixture>, Task<crate::app::Message>), String> {
    let Some(launch) = acceptance::Launch::from_env()? else {
        return Ok((None, Task::none()));
    };
    let (fixture, task) = acceptance::Fixture::new(
        launch,
        POINTS,
        vec![inspect::Target::new("root", widget::Id::from(ROOT_ID))],
        inspect::Limits::new(),
    )?;
    Ok((Some(fixture), task))
}
