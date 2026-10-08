// SPDX-License-Identifier: MIT OR Apache-2.0
//! The shared fixture on Cap's existing main window and native worker.
use application::{acceptance, iced::{Task, widget}, inspect};

pub const ROOT_ID: &str = "cap.fixture.root";
pub const VIEWPORT_ID: &str = "cap.fixture.viewport";
pub const POINTS: &[&str] = &["cap.prepare"];

pub(crate) fn setup() -> Result<(Option<acceptance::Fixture>, Task<crate::app::Message>), String> {
    let Some(launch) = acceptance::Launch::from_env()? else {
        return Ok((None, Task::none()));
    };
    let (fixture, task) = acceptance::Fixture::new(launch, POINTS, vec![
        inspect::Target::new("root", widget::Id::from(ROOT_ID)),
        inspect::Target::new("viewport", widget::Id::from(VIEWPORT_ID)),
    ], inspect::Limits::new())?;
    Ok((Some(fixture), task))
}
