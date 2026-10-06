//! Agent-first frame capture: no input-authority or active-VT gate.

use comp_model::capture::CaptureFrameSpec;
use comp_model::reply::ControlReply;
use dispatcher::state::state::RedrawReason;
use dispatcher::wire::trait_::wire_trait::WireTrait;
use serde_json::json;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use std::time::{Duration, Instant};
use world::state::Loop;

pub fn start(
    lp: &mut Loop,
    spec: CaptureFrameSpec,
    admitted: Instant,
    answer: impl FnOnce(ControlReply) + 'static,
) {
    let window_output = if let Some(target) = spec.window {
        let id = match lp
            .inner
            .comp
            .registry
            .resolve_window_target(target.id, Some(target.generation))
        {
            Ok(record) => record.id(),
            Err(error) => {
                answer(ControlReply::WindowTarget {
                    id: target.id,
                    error,
                });
                return;
            }
        };
        let Some(window) = crate::control::window_of(lp, id) else {
            answer(ControlReply::WindowTarget {
                id: target.id,
                error: surfaces::WindowTargetError::NotMapped,
            });
            return;
        };
        // Resolve in its owning world, even if that world is currently parked.
        lp.inner.all_world_spaces().iter().find_map(|space| {
            space.state.element_location(&window)?;
            space.state.outputs_for_element(&window).first().cloned()
        })
    } else {
        None
    };
    let output = lp
        .inner
        .space_state()
        .state
        .outputs()
        .find(|output| {
            if let Some(window_output) = &window_output {
                *output == window_output
            } else {
                spec.output
                    .as_ref()
                    .is_none_or(|name| *name == output.name())
            }
        })
        .cloned();
    let Some(output) = output else {
        answer(ControlReply::Refused {
            error: "unknown_output",
            detail: json!({"output": spec.output}),
        });
        return;
    };
    if spec.region.is_some() && output.current_transform()!=smithay::utils::Transform::Normal {
        answer(ControlReply::refused("unsupported_transform",json!({"output":output.name(),"message":"region capture requires an unrotated output"})));
        return;
    }
    if spec
        .output_generation
        .is_some_and(|expected| expected != lp.inner.comp.output_generation(&output.name()))
    {
        answer(ControlReply::refused(
            "output_changed",
            json!({"output":output.name()}),
        ));
        return;
    }
    let id = screencopy::file::enqueue(&output, spec, move |result| {
        answer(match result {
            Ok(body) => ControlReply::Body(body),
            Err(reply) => reply,
        });
    });
    // A stopped/dark renderer, a removed output or a failed render must always
    // answer. Timer and render completion race only on this compositor thread;
    // taking the request makes the second completion a no-op.
    if lp
        .loop_handle
        .insert_source(
            Timer::from_deadline(admitted + Duration::from_secs(3)),
            move |_, _, _| {
                screencopy::file::fail(id, "capture deadline passed");
                TimeoutAction::Drop
            },
        )
        .is_err()
    {
        screencopy::file::fail(id, "capture deadline timer unavailable");
        return;
    }
    lp.state.redraw.request_for(RedrawReason::Capture);
    lp.state.redraw.wake();
}
