//! One-shot Bus file captures, serviced on the compositor's render thread.

use crate::{Readback, Source, source_row, start_readback};
pub use comp_model::capture::CaptureSource;
use comp_model::capture::{CaptureFormat, CaptureFrameSpec, CaptureWindow};
pub use comp_model::reply::ControlReply;
use serde_json::Value;
use smithay::backend::renderer::ExportMem;
use smithay::output::Output;
use smithay::utils::Rectangle;
use std::cell::RefCell;
use std::io::Write;

type Answer = Box<dyn FnOnce(Result<Value, ControlReply>)>;
struct Request {
    id: u64,
    output: String,
    spec: CaptureFrameSpec,
    answer: Answer,
}

thread_local! {
    static REQUESTS: RefCell<Vec<Request>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

pub fn enqueue(
    output: &Output,
    spec: CaptureFrameSpec,
    answer: impl FnOnce(Result<Value, ControlReply>) + 'static,
) -> u64 {
    let id = NEXT_ID.with(|next| {
        let id = next.get();
        next.set(id.wrapping_add(1));
        id
    });
    REQUESTS.with_borrow_mut(|requests| {
        requests.push(Request {
            id,
            output: output.name(),
            spec,
            answer: Box::new(answer),
        })
    });
    id
}

pub fn pending(output: &Output) -> bool {
    REQUESTS.with_borrow(|requests| {
        requests
            .iter()
            .any(|request| request.output == output.name())
    })
}

pub fn pending_output(output: &Output) -> bool {
    REQUESTS.with_borrow(|requests| {
        requests
            .iter()
            .any(|request| request.output == output.name() && request.spec.window.is_none())
    })
}

pub fn pending_windows() -> bool {
    REQUESTS.with_borrow(|requests| requests.iter().any(|request| request.spec.window.is_some()))
}

/// Window requests never consume an output framebuffer, including the KMS tap.
pub(crate) fn service_windows(
    output: &Output,
    mut capture: impl FnMut(CaptureWindow) -> Result<(Readback, Vec<u8>), ControlReply>,
) {
    for request in take(|request| request.output == output.name() && request.spec.window.is_some())
    {
        let target = request.spec.window.expect("window request");
        let result = capture(target).and_then(|(readback, pixels)| {
            complete(
                &request.spec,
                readback,
                &pixels,
                output,
                CaptureSource::Offscreen,
            )
            .map_err(capture_failed)
        });
        (request.answer)(result);
    }
}

pub fn capture_failed(message: impl Into<String>) -> ControlReply {
    ControlReply::Refused {
        error: "capture_failed",
        detail: serde_json::json!({"message": message.into()}),
    }
}

fn take(keep: impl Fn(&Request) -> bool) -> Vec<Request> {
    REQUESTS.with_borrow_mut(|requests| {
        let (taken, rest) = std::mem::take(requests).into_iter().partition(keep);
        *requests = rest;
        taken
    })
}

pub fn fail(id: u64, reason: &str) {
    for request in take(|request| request.id == id) {
        (request.answer)(Err(capture_failed(reason)));
    }
}

pub fn fail_output(output: &Output, reason: &str) {
    for request in take(|request| request.output == output.name()) {
        (request.answer)(Err(capture_failed(reason)));
    }
}

pub(crate) fn fail_output_capture(output: &Output, reason: &str) {
    for request in take(|request| request.output == output.name() && request.spec.window.is_none())
    {
        (request.answer)(Err(capture_failed(reason)));
    }
}

pub fn fail_all(reason: &str) {
    for request in take(|_| true) {
        (request.answer)(Err(capture_failed(reason)));
    }
}

pub fn fail_windows(reason: &str) {
    for request in take(|request| request.spec.window.is_some()) {
        (request.answer)(Err(capture_failed(reason)));
    }
}

/// Read once for all callers, normalise to top-down RGB, then encode. Mapping
/// may change the current GL target; nested restores its EGL surface afterwards.
pub fn service<R: ExportMem>(
    renderer: &mut R,
    source: Source<'_, '_, R>,
    output: &Output,
    kind: CaptureSource,
) {
    let requests = take(|request| request.output == output.name() && request.spec.window.is_none());
    if requests.is_empty() {
        return;
    }
    let readback = source.readback;
    let pixels = read_pixels(renderer, source);
    for request in requests {
        let result = pixels
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|pixels| complete(&request.spec, readback, pixels, output, kind))
            .map_err(capture_failed);
        (request.answer)(result);
    }
}

pub(crate) fn read_pixels<R: ExportMem>(
    renderer: &mut R,
    source: Source<'_, '_, R>,
) -> Result<Vec<u8>, String> {
    let Readback {
        size,
        origin_bottom_left,
    } = source.readback;
    if size.w <= 0 || size.h <= 0 {
        return Err("capture has no usable size".to_owned());
    }
    let (mapping, rgba) = start_readback(
        renderer,
        source.framebuffer,
        Rectangle::from_size(size),
        &source.readback,
    )?;
    let bytes = renderer
        .map_texture(&mapping)
        .map_err(|err| format!("map capture: {err}"))?;
    rgb(
        bytes,
        size.w as usize,
        size.h as usize,
        rgba,
        origin_bottom_left,
    )
}

fn complete(
    spec: &CaptureFrameSpec,
    readback: Readback,
    pixels: &[u8],
    output: &Output,
    kind: CaptureSource,
) -> Result<Value, String> {
    let size = readback.size;
    write(spec, size.w as u32, size.h as u32, pixels)?;
    Ok(comp_model::capture::reply(
        &spec.path,
        size.w as u32,
        size.h as u32,
        output.current_scale().fractional_scale(),
        &output.name(),
        spec.window,
        kind,
    ))
}

fn rgb(
    bytes: &[u8],
    width: usize,
    height: usize,
    rgba: bool,
    bottom_up: bool,
) -> Result<Vec<u8>, String> {
    let row = width.checked_mul(4).ok_or("capture size overflow")?;
    let len = row.checked_mul(height).ok_or("capture size overflow")?;
    if bytes.len() < len {
        return Err("capture readback is shorter than its layout".into());
    }
    let mut pixels = Vec::with_capacity(len / 4 * 3);
    for y in 0..height {
        let at = source_row(y, height, bottom_up) * row;
        for px in bytes[at..at + row].chunks_exact(4) {
            if rgba {
                pixels.extend_from_slice(&px[..3]);
            } else {
                pixels.extend_from_slice(&[px[2], px[1], px[0]]);
            }
        }
    }
    Ok(pixels)
}

fn write(spec: &CaptureFrameSpec, width: u32, height: u32, pixels: &[u8]) -> Result<(), String> {
    match spec.format {
        CaptureFormat::Png => image::save_buffer_with_format(
            &spec.path,
            pixels,
            width,
            height,
            image::ColorType::Rgb8,
            image::ImageFormat::Png,
        )
        .map_err(|err| format!("write PNG: {err}")),
        CaptureFormat::Ppm => {
            let mut file =
                std::fs::File::create(&spec.path).map_err(|err| format!("create PPM: {err}"))?;
            write!(file, "P6\n{width} {height}\n255\n")
                .and_then(|_| file.write_all(pixels))
                .and_then(|_| file.flush())
                .map_err(|err| format!("write PPM: {err}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readback_channel_order_and_orientation() {
        let bgra = [3, 2, 1, 255, 6, 5, 4, 255];
        assert_eq!(rgb(&bgra, 1, 2, false, false).unwrap(), [1, 2, 3, 4, 5, 6]);
        assert_eq!(rgb(&bgra, 1, 2, false, true).unwrap(), [4, 5, 6, 1, 2, 3]);
        assert_eq!(rgb(&bgra, 1, 2, true, false).unwrap(), [3, 2, 1, 6, 5, 4]);
        assert!(rgb(&bgra[..3], 1, 1, false, false).is_err());
    }

    #[test]
    fn window_service_preserves_output_requests_and_target_refusals() {
        use smithay::output::{PhysicalProperties, Subpixel};
        use std::rc::Rc;
        let output = Output::new(
            "capture-test".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
                serial_number: "test".into(),
            },
        );
        let answers = Rc::new(RefCell::new(Vec::new()));
        let got = answers.clone();
        let spec = comp_model::capture::parse(&serde_json::json!({
            "path": "/tmp/window.png", "window": {"id": 7, "generation": 3}
        }))
        .unwrap();
        let window_id = enqueue(&output, spec, move |answer| got.borrow_mut().push(answer));
        assert!(pending(&output));
        assert!(pending_windows());
        assert!(!pending_output(&output));
        let got = answers.clone();
        let spec =
            comp_model::capture::parse(&serde_json::json!({"path": "/tmp/output.png"})).unwrap();
        let output_id = enqueue(&output, spec, move |answer| got.borrow_mut().push(answer));
        service_windows(&output, |target| {
            assert_eq!(
                target,
                CaptureWindow {
                    id: 7,
                    generation: 3
                }
            );
            Err(ControlReply::refused(
                "stale_target",
                serde_json::json!({
                    "id": 7, "generation": 3, "current": 4
                }),
            ))
        });
        assert!(pending_output(&output));
        assert!(!pending_windows());
        let reply = answers.borrow_mut().pop().unwrap().unwrap_err().into_wire();
        let body: Value = serde_json::from_str(&reply.1).unwrap();
        assert_eq!(reply.0, 10);
        assert_eq!(body["error"], "stale_target");
        assert_eq!(body["current"], 4);
        fail(window_id, "deadline after completion");
        assert!(answers.borrow().is_empty());
        fail(output_id, "output deadline");
        assert_eq!(answers.borrow().len(), 1);
        assert!(!pending(&output));
    }

    #[test]
    fn failed_or_expired_requests_are_answered_once() {
        use smithay::output::{PhysicalProperties, Subpixel};
        use std::rc::Rc;
        let output = Output::new(
            "DP-1".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
                serial_number: "test".into(),
            },
        );
        let answers = Rc::new(RefCell::new(Vec::new()));
        let got = answers.clone();
        let spec =
            comp_model::capture::parse(&serde_json::json!({"path": "/tmp/frame.png"})).unwrap();
        let id = enqueue(&output, spec, move |answer| got.borrow_mut().push(answer));
        assert!(pending(&output));
        fail(id, "deadline");
        fail_output(&output, "render failed");
        fail(id, "deadline again");
        assert!(!pending(&output));
        let answers = answers.borrow();
        assert_eq!(answers.len(), 1);
        let Err(ControlReply::Refused { error, detail }) = &answers[0] else {
            panic!("expected refusal")
        };
        assert_eq!(*error, "capture_failed");
        assert_eq!(detail["message"], "deadline");
    }
}
