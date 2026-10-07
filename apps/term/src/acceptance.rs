// SPDX-License-Identifier: MIT OR Apache-2.0
//! Explicit owned fixture wiring on the existing application and Bus workers.

use application::{acceptance::{self, barrier}, iced::{Task, widget}, inspect};

pub const ROOT_ID: &str = "term.fixture.root";
pub const POINTS: &[&str] = &["terminal.prepare"];

pub(crate) struct Fixture {
    pub describe: acceptance::Describe,
    pub inspector: inspect::Handle,
    pub controller: barrier::Controller,
    pub hook: barrier::Hook,
}

impl Fixture {
    pub fn close(&self, reason: barrier::ClosedReason) {
        self.controller.close(reason);
        if reason != barrier::ClosedReason::LostGeneration { self.inspector.close(); }
    }
}

pub(crate) fn setup() -> Result<(Option<Fixture>, Task<crate::Message>), String> {
    let Some(launch) = acceptance::Launch::from_env()? else { return Ok((None, Task::none())); };
    let (fixture,task) = setup_launch(launch)?;
    Ok((Some(fixture),task))
}

pub(crate) fn setup_launch(launch: acceptance::Launch) -> Result<(Fixture, Task<crate::Message>), String> {
    let limits = inspect::Limits::new();
    let (inspector, task) = inspect::channel(vec![inspect::Target::new("root",widget::Id::from(ROOT_ID))], limits)
        .map_err(|error| format!("fixture inspector: {error:?}"))?;
    let run = barrier::Run::new(launch.run.clone(), launch.instance).map_err(|error| format!("fixture run: {error:?}"))?;
    let (controller,hook) = barrier::barrier(POINTS,run);
    let describe = acceptance::Describe::new(std::process::id(),launch.run,launch.instance)
        .map_err(|error| format!("fixture identity: {error:?}"))?
        .points(POINTS.iter().map(|point| (*point).to_owned()).collect())
        .aliases(vec!["root".to_owned()]).limits(limits);
    Ok((Fixture {describe,inspector,controller,hook},task))
}
