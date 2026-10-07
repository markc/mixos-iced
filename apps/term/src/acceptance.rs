// SPDX-License-Identifier: MIT OR Apache-2.0
//! Explicit owned fixture wiring on the existing application and Bus workers.

use application::{
    acceptance,
    iced::{Task, widget},
    inspect,
};

pub const ROOT_ID: &str = "term.fixture.root";
pub const POINTS: &[&str] = &["terminal.prepare"];

pub(crate) use acceptance::Fixture;

pub(crate) fn setup() -> Result<(Option<Fixture>, Task<crate::Message>), String> {
    let Some(launch) = acceptance::Launch::from_env()? else {
        return Ok((None, Task::none()));
    };
    let (fixture, task) = setup_launch(launch)?;
    Ok((Some(fixture), task))
}

pub(crate) fn setup_launch(
    launch: acceptance::Launch,
) -> Result<(Fixture, Task<crate::Message>), String> {
    Fixture::new(
        launch,
        POINTS,
        vec![inspect::Target::new("root", widget::Id::from(ROOT_ID))],
        inspect::Limits::new(),
    )
}
