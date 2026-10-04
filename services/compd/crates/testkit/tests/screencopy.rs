//! wlr-screencopy v3 through compd's real Dispatch, serviced the way a backend
//! services it after a frame (`Harness::service_screencopy`, a top-down pixman
//! framebuffer standing in for the renderer).

use testkit::Harness;
use testkit::client::FrameEvents;
use smithay::utils::{Physical, Rectangle};
use wayland_client::protocol::wl_shm;

/// `wl_shm.format` XRGB8888.
const XRGB8888: u32 = 1;

/// A pixel that encodes where it is: red = x low byte, green = y low byte.
fn coded(x: i32, y: i32) -> u32 {
    (((x & 0xff) as u32) << 16) | (((y & 0xff) as u32) << 8)
}

/// The ledger's `capture` request count: the redraws screencopy asked for.
fn capture_requests(h: &Harness) -> u64 {
    h.wire.state.redraw.ledger().snapshot().reasons["capture"].requests
}

fn frame(h: &Harness, index: usize) -> FrameEvents {
    h.client.state.frames[index].clone()
}

#[test]
fn the_whole_output_is_advertised_as_one_xrgb_shm_layout() {
    let mut h = Harness::new();
    let (_frame, i) = h.client.capture(None);
    h.roundtrip();
    let f = frame(&h, i);
    assert_eq!(f.buffer, Some((XRGB8888, 1920, 1080, 1920 * 4)));
    assert!(f.buffer_done, "v3 ends the advertisement with buffer_done");
    assert!(!f.failed);
}

#[test]
fn regions_are_clipped_and_empty_ones_fail() {
    let mut h = Harness::new();
    let (_a, a) = h.client.capture(Some((1900, 1070, 100, 100)));
    let (_b, b) = h.client.capture(Some((0, 0, 0, 10)));
    let (_c, c) = h.client.capture(Some((5000, 0, 10, 10)));
    h.roundtrip();
    assert_eq!(
        frame(&h, a).buffer,
        Some((XRGB8888, 20, 10, 80)),
        "clipped to the output"
    );
    assert!(frame(&h, b).failed, "an empty region fails");
    assert!(frame(&h, c).failed, "a region outside the output fails");
}

#[test]
fn copy_requests_one_frame_and_reads_it_top_down() {
    let mut h = Harness::new();
    let (frame_obj, i) = h.client.capture(Some((10, 20, 4, 3)));
    h.roundtrip();
    let (buffer, file) = h.client.shm_buffer_with(4, 3, 16, wl_shm::Format::Xrgb8888);
    let before = capture_requests(&h);
    frame_obj.copy(&buffer);
    h.roundtrip();
    assert_eq!(
        capture_requests(&h),
        before + 1,
        "a copy asks for exactly one frame"
    );
    assert!(!frame(&h, i).ready, "nothing is read until a frame renders");
    h.service_screencopy(coded, None);
    let f = frame(&h, i);
    assert!(f.ready, "{f:?}");
    assert_eq!(f.flags, Some(0), "top-down: no y_invert");
    let bytes = h.client.read_buffer(file);
    for y in 0..3 {
        for x in 0..4 {
            let at = (y * 16 + x * 4) as usize;
            // XRGB8888 in memory: B, G, R, X.
            assert_eq!(bytes[at + 2], (10 + x) as u8, "red = output x at ({x},{y})");
            assert_eq!(
                bytes[at + 1],
                (20 + y) as u8,
                "green = output y at ({x},{y}): row order"
            );
        }
    }
}

#[test]
fn a_second_copy_of_one_frame_is_already_used() {
    let mut h = Harness::new();
    let (frame_obj, _) = h.client.capture(Some((0, 0, 2, 2)));
    h.roundtrip();
    let (buffer, _) = h.client.shm_buffer_with(2, 2, 8, wl_shm::Format::Xrgb8888);
    frame_obj.copy(&buffer);
    frame_obj.copy(&buffer);
    let error = h.roundtrip_expecting_error();
    assert_eq!(error.code, 0, "already_used: {error:?}");
}

#[test]
fn a_buffer_that_does_not_match_the_layout_is_invalid() {
    let mut h = Harness::new();
    let (frame_obj, _) = h.client.capture(Some((0, 0, 2, 2)));
    h.roundtrip();
    let (buffer, _) = h.client.shm_buffer_with(3, 2, 12, wl_shm::Format::Xrgb8888);
    frame_obj.copy(&buffer);
    let error = h.roundtrip_expecting_error();
    assert_eq!(error.code, 1, "invalid_buffer: {error:?}");
}

/// The idle rule: a damage copy with nothing owed asks for NO frame, and is
/// answered by the first frame that damages its region.
#[test]
fn copy_with_damage_waits_for_damage_without_asking_for_frames() {
    let mut h = Harness::new();
    let region = Some((100, 100, 10, 10));
    // First damage copy: no baseline, answered at once with the whole region.
    let (first, a) = h.client.capture(region);
    h.roundtrip();
    let (buf_a, _) = h
        .client
        .shm_buffer_with(10, 10, 40, wl_shm::Format::Xrgb8888);
    first.copy_with_damage(&buf_a);
    h.roundtrip();
    h.service_screencopy(coded, None);
    assert!(frame(&h, a).ready);
    assert_eq!(frame(&h, a).damage, [(0, 0, 10, 10)]);

    // Second: nothing owed, so it waits and asks for nothing.
    let (second, b) = h.client.capture(region);
    h.roundtrip();
    let (buf_b, _) = h
        .client
        .shm_buffer_with(10, 10, 40, wl_shm::Format::Xrgb8888);
    let before = capture_requests(&h);
    second.copy_with_damage(&buf_b);
    h.roundtrip();
    assert_eq!(
        capture_requests(&h),
        before,
        "no frame asked for while nothing is damaged"
    );
    h.service_screencopy(coded, None);
    assert!(
        !frame(&h, b).ready,
        "a frame without damage does not answer it"
    );
    let elsewhere: [Rectangle<i32, Physical>; 1] =
        [Rectangle::new((500, 500).into(), (5, 5).into())];
    h.service_screencopy(coded, Some(&elsewhere));
    assert!(
        !frame(&h, b).ready,
        "damage outside the region does not either"
    );
    let inside: [Rectangle<i32, Physical>; 1] = [Rectangle::new((104, 102).into(), (3, 2).into())];
    h.service_screencopy(coded, Some(&inside));
    let f = frame(&h, b);
    assert!(f.ready, "{f:?}");
    // The damage since the first copy, clipped and region-local: the outside
    // rect is dropped, the inside one reported.
    assert_eq!(f.damage, [(4, 2, 3, 2)]);
}

#[test]
fn a_client_holds_at_most_four_live_frames() {
    let mut h = Harness::new();
    let indices: Vec<usize> = (0..5)
        .map(|_| h.client.capture(Some((0, 0, 1, 1))).1)
        .collect();
    h.roundtrip();
    for &i in &indices[..4] {
        assert!(!frame(&h, i).failed);
    }
    assert!(frame(&h, indices[4]).failed, "the fifth live frame fails");
}

/// A pixel the `coded` picture never has: stands in for the cursor.
const CURSOR: u32 = 0x00ff_00ff;

fn read_pixel(bytes: &[u8], stride: usize, x: usize, y: usize) -> u32 {
    let at = y * stride + x * 4;
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], 0])
}

/// `overlay_cursor = 0` (the default, as grim asks): the copy reads the frame
/// rendered WITHOUT its cursor, which the backend renders only because the copy
/// is due.
#[test]
fn a_cursorless_copy_reads_the_frame_without_its_cursor() {
    let mut h = Harness::new();
    let (frame_obj, i) = h.client.capture_with(Some((10, 20, 4, 3)), false);
    h.roundtrip();
    let (buffer, file) = h.client.shm_buffer_with(4, 3, 16, wl_shm::Format::Xrgb8888);
    frame_obj.copy(&buffer);
    h.roundtrip();
    let rendered = h.service_screencopy_with(&|_, _| CURSOR, &coded, None);
    assert!(
        rendered,
        "a due cursorless copy asks for the cursorless frame"
    );
    assert!(frame(&h, i).ready);
    let bytes = h.client.read_buffer(file);
    for y in 0..3 {
        for x in 0..4 {
            assert_eq!(
                read_pixel(&bytes, 16, x, y),
                coded(10 + x as i32, 20 + y as i32),
                "the cursorless picture at ({x},{y})"
            );
        }
    }
}

/// `overlay_cursor = 1`: the copy reads the frame as shown, and no cursorless
/// frame is rendered for it.
#[test]
fn a_cursor_copy_reads_the_frame_as_shown() {
    let mut h = Harness::new();
    let (frame_obj, i) = h.client.capture_with(Some((0, 0, 2, 2)), true);
    h.roundtrip();
    let (buffer, file) = h.client.shm_buffer_with(2, 2, 8, wl_shm::Format::Xrgb8888);
    frame_obj.copy(&buffer);
    h.roundtrip();
    let rendered = h.service_screencopy_with(&|_, _| CURSOR, &coded, None);
    assert!(!rendered, "no copy needs a cursorless frame");
    assert!(frame(&h, i).ready);
    let bytes = h.client.read_buffer(file);
    assert_eq!(read_pixel(&bytes, 8, 1, 1), CURSOR);
}

/// The second render is paid only by frames that serve a cursorless copy: a
/// waiting damage copy asks for it only on the frame that damages its region.
#[test]
fn the_cursorless_frame_is_rendered_only_when_a_cursorless_copy_is_due() {
    let mut h = Harness::new();
    let region = Some((100, 100, 10, 10));
    let (first, _) = h.client.capture(region);
    h.roundtrip();
    let (buf_a, _) = h
        .client
        .shm_buffer_with(10, 10, 40, wl_shm::Format::Xrgb8888);
    first.copy_with_damage(&buf_a);
    h.roundtrip();
    assert!(
        h.service_screencopy_with(&coded, &coded, None),
        "first damage copy: due at once"
    );

    let (second, b) = h.client.capture(region);
    h.roundtrip();
    let (buf_b, _) = h
        .client
        .shm_buffer_with(10, 10, 40, wl_shm::Format::Xrgb8888);
    second.copy_with_damage(&buf_b);
    h.roundtrip();
    assert!(
        !h.service_screencopy_with(&coded, &coded, None),
        "no damage: not due"
    );
    let elsewhere: [Rectangle<i32, Physical>; 1] =
        [Rectangle::new((500, 500).into(), (5, 5).into())];
    assert!(
        !h.service_screencopy_with(&coded, &coded, Some(&elsewhere)),
        "damage outside the region: not due"
    );
    assert!(!frame(&h, b).ready);
    let inside: [Rectangle<i32, Physical>; 1] = [Rectangle::new((104, 102).into(), (3, 2).into())];
    assert!(
        h.service_screencopy_with(&coded, &coded, Some(&inside)),
        "damage inside the region: due, so rendered"
    );
    assert!(frame(&h, b).ready);
}

/// A backend that drops a frame's captures without finishing them (it lost the
/// frame) answers every copy they served `failed`, never leaving one hanging.
#[test]
fn captures_dropped_unfinished_answer_failed() {
    let mut h = Harness::new();
    let (frame_obj, i) = h.client.capture(Some((0, 0, 2, 2)));
    h.roundtrip();
    let (buffer, _) = h.client.shm_buffer_with(2, 2, 8, wl_shm::Format::Xrgb8888);
    frame_obj.copy(&buffer);
    h.roundtrip();
    h.service_screencopy_unfinished(coded);
    let f = frame(&h, i);
    assert!(f.failed, "{f:?}");
    assert!(!f.ready);
}

/// The native backend renders a picture only when a copy is due: nothing at
/// all with no copy pending, or with a damage copy waiting and nothing damaged.
#[test]
fn native_renders_no_picture_while_no_copy_is_due() {
    let mut h = Harness::new();
    assert_eq!(
        h.service_screencopy_native(&coded, &coded, None),
        screencopy::SourcesDue::default(),
        "no copy pending"
    );
    let region = Some((0, 0, 8, 8));
    let (first, _) = h.client.capture(region);
    h.roundtrip();
    let (buf_a, _) = h.client.shm_buffer_with(8, 8, 32, wl_shm::Format::Xrgb8888);
    first.copy_with_damage(&buf_a);
    h.roundtrip();
    h.service_screencopy_native(&coded, &coded, None);
    let (second, _) = h.client.capture(region);
    h.roundtrip();
    let (buf_b, _) = h.client.shm_buffer_with(8, 8, 32, wl_shm::Format::Xrgb8888);
    second.copy_with_damage(&buf_b);
    h.roundtrip();
    assert_eq!(
        h.service_screencopy_native(&coded, &coded, None),
        screencopy::SourcesDue::default(),
        "a waiting damage copy with nothing damaged"
    );
}

/// With both kinds due on one frame, the native backend renders both pictures
/// and each copy reads the one it asked for.
#[test]
fn native_serves_each_copy_the_picture_it_asked_for() {
    let mut h = Harness::new();
    let (with, a) = h.client.capture_with(Some((0, 0, 2, 2)), true);
    let (without, b) = h.client.capture_with(Some((0, 0, 2, 2)), false);
    h.roundtrip();
    let (buf_a, file_a) = h.client.shm_buffer_with(2, 2, 8, wl_shm::Format::Xrgb8888);
    let (buf_b, file_b) = h.client.shm_buffer_with(2, 2, 8, wl_shm::Format::Xrgb8888);
    with.copy(&buf_a);
    without.copy(&buf_b);
    h.roundtrip();
    let due = h.service_screencopy_native(&|_, _| CURSOR, &coded, None);
    assert_eq!(
        due,
        screencopy::SourcesDue {
            cursor: true,
            cursorless: true
        }
    );
    assert!(frame(&h, a).ready && frame(&h, b).ready);
    assert_eq!(read_pixel(&h.client.read_buffer(file_a), 8, 1, 1), CURSOR);
    assert_eq!(
        read_pixel(&h.client.read_buffer(file_b), 8, 1, 1),
        coded(1, 1)
    );
}
