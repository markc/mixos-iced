//! screencopy: wlr-screencopy (`zwlr_screencopy_manager_v1`, version 3) for
//! compd.
//!
//! Two halves:
//!
//! - **Protocol** ([`create_global`]): the manager and frame objects, running on
//!   compd's real `Dispatch` through smithay's `Dispatch2` blanket (so no handler
//!   impl in dispatcher is needed). A frame advertises one shm layout —
//!   `XRGB8888`, the capture region's physical size, tight stride — then
//!   `buffer_done` (v3). No `linux_dmabuf` is advertised yet: shm only.
//! - **Service**, in two steps. [`service`] is called by a backend for EVERY
//!   output frame it renders, after the render and before it hands the frame
//!   away: it starts the readback of each copy due from the framebuffer it just
//!   rendered and returns them as [`Captures`]. [`Captures::finish`] is called
//!   AFTER the frame is handed away (swapped, queued): it maps the readbacks,
//!   fills the clients' shm buffers and answers the frames. The split is not
//!   optional: mapping (`ExportMem::map_texture`) may change what is current
//!   on the renderer — GLES makes its context current with NO surface — and an
//!   EGL window surface that is no longer current cannot be swapped
//!   (`EGL_BAD_SURFACE`).
//!
//! Idle, by construction. Nothing here polls or runs a timer:
//! - `copy` asks for ONE frame (reason `Capture`) and [`wants_full_frame`] tells
//!   the backend to render it in full, so the readback is the whole current
//!   picture even on a static screen;
//! - `copy_with_damage` asks for nothing when no damage is owed: it waits for a
//!   frame that something else caused, and is serviced by the first one that
//!   carries damage in its output. A client's first damage copy is answered at
//!   once with the whole region as damage (there is no earlier frame to diff
//!   against), as wlroots does.
//!
//! Damage reported is the damage accumulated for that client on that output
//! since its previous damage copy, clipped to the region, in region-local
//! physical pixels.
//!
//! `overlay_cursor`: a copy that asks for no cursor excludes cursor and drag
//! icon elements ([`sources_due`] tells the backend when to make that picture).
//! A cursor baked into the presentation buffer requires a separate render
//! ([`render_offscreen`]); a native frame with separate cursor planes can filter
//! them when copying its frame result. The picture shown keeps its cursor.
//!
//! Backends: the nested one reads its window surface (bottom-up) for copies
//! with the cursor and asks for full frames while a copy is owed
//! ([`wants_full_frame`]); native copies the KMS frame result (primary buffer
//! plus promoted planes) into an [`offscreen_texture`], replaying the elements
//! only to remove a cursor baked into the primary. It takes the damage from
//! [`frame_damage`].
//!
//! Not done yet: DMA-BUF destinations; rotated or flipped outputs (the regions
//! and the offscreen pictures are untransformed, so a copy of a transformed
//! output is not in its buffer orientation and carries no `y_invert`).

#[macro_use]
extern crate model;

use std::cell::RefCell;

use dispatcher::state::state::Dispatch;
use protocols::redraw::schedule::schedule::RedrawReason;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::ExportMem;
use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
};
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::{Client, DataInit, DisplayHandle, New, Resource};
use smithay::utils::{Buffer as BufferCoords, Logical, Physical, Rectangle, Size};
use smithay::wayland::{Dispatch2, GlobalDispatch2};

/// The protocol version advertised.
pub const VERSION: u32 = 3;
/// Live (created, not yet finished) frames one client may hold.
pub const MAX_CLIENT_FRAMES: usize = 4;
/// Live frames across every client.
pub const MAX_FRAMES: usize = 32;
/// Damage accumulators kept (one per client and output that has used
/// `copy_with_damage`); the oldest is dropped beyond this.
const MAX_ACCUMULATORS: usize = 64;
/// Damage rectangles kept per accumulator before they are merged into one box.
const MAX_ACCUMULATED_RECTS: usize = 16;

/// A copy a frame serves: record id, client, region, overlay_cursor, and the
/// submitted copy.
type Served = (u64, ClientId, Rectangle<i32, Physical>, bool, Pending);

/// A submitted copy, waiting for a frame.
struct Pending {
    frame: ZwlrScreencopyFrameV1,
    buffer: WlBuffer,
    with_damage: bool,
    /// Needs the next frame in full ([`wants_full_frame`]): a plain copy, or a
    /// damage copy whose damage is already owed.
    immediate: bool,
}

struct Record {
    id: u64,
    client: ClientId,
    output: String,
    region: Rectangle<i32, Physical>,
    stride: i32,
    /// The client asked for the cursor (and drag icon) in the copy.
    overlay_cursor: bool,
    submitted: bool,
    pending: Option<Pending>,
}

/// Damage seen on `output` since `client`'s last damage copy there.
struct Accumulator {
    client: ClientId,
    output: String,
    rects: Vec<Rectangle<i32, Physical>>,
}

#[derive(Default)]
struct Registry {
    next_id: u64,
    records: Vec<Record>,
    accumulators: Vec<Accumulator>,
}

pub mod file;
pub mod offscreen;

thread_local! {
    // The compositor is single-threaded; the protocol and the backend that
    // services copies run on the same thread. Per thread, so each headless test
    // harness (one per test thread) has its own.
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

// ── Protocol ────────────────────────────────────────────────────────────────

/// The manager global's data.
pub struct ManagerGlobal;
/// A bound manager's data.
pub struct ManagerData;
/// A frame's data: its key in the registry.
pub struct FrameData {
    pub id: u64,
}

/// Advertise `zwlr_screencopy_manager_v1` (version [`VERSION`]).
pub fn create_global(display: &DisplayHandle) -> GlobalId {
    display
        .create_global::<Dispatch, ZwlrScreencopyManagerV1, ManagerGlobal>(VERSION, ManagerGlobal)
}

impl GlobalDispatch2<ZwlrScreencopyManagerV1, Dispatch> for ManagerGlobal {
    fn bind(
        &self,
        _state: &mut Dispatch,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        data_init: &mut DataInit<'_, Dispatch>,
    ) {
        data_init.init(resource, ManagerData);
    }
}

impl Dispatch2<ZwlrScreencopyManagerV1, Dispatch> for ManagerData {
    fn request(
        &self,
        _state: &mut Dispatch,
        client: &Client,
        _resource: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Dispatch>,
    ) {
        match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput {
                frame,
                overlay_cursor,
                output,
            } => {
                capture(client, frame, &output, None, overlay_cursor != 0, data_init);
            }
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion {
                frame,
                overlay_cursor,
                output,
                x,
                y,
                width,
                height,
            } => {
                let logical = Rectangle::<i32, Logical>::new((x, y).into(), (width, height).into());
                capture(
                    client,
                    frame,
                    &output,
                    Some(logical),
                    overlay_cursor != 0,
                    data_init,
                );
            }
            // A destroyed manager leaves its frames usable.
            zwlr_screencopy_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch2<ZwlrScreencopyFrameV1, Dispatch> for FrameData {
    fn request(
        &self,
        state: &mut Dispatch,
        client: &Client,
        resource: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Dispatch>,
    ) {
        match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => {
                submit(state, client, resource, self.id, buffer, false);
            }
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => {
                submit(state, client, resource, self.id, buffer, true);
            }
            // `destroyed` below forgets it.
            zwlr_screencopy_frame_v1::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        &self,
        _state: &mut Dispatch,
        _client: ClientId,
        _resource: &ZwlrScreencopyFrameV1,
    ) {
        REGISTRY.with_borrow_mut(|r| r.records.retain(|record| record.id != self.id));
    }
}

/// `region` (output-local logical coordinates, `None` = the whole output) in the
/// output's physical pixels, clipped to the output. `None` when the output has
/// no mode or the region is empty or entirely outside it.
pub fn physical_region(
    output: &Output,
    logical: Option<Rectangle<i32, Logical>>,
) -> Option<Rectangle<i32, Physical>> {
    let mode = output.current_mode()?;
    let full = Rectangle::<i32, Physical>::from_size(mode.size);
    let Some(logical) = logical else {
        return Some(full);
    };
    if logical.size.w <= 0 || logical.size.h <= 0 {
        return None;
    }
    let scale = output.current_scale().fractional_scale();
    scaled_region(logical, scale, full)
}

/// The logical→physical step of [`physical_region`]: outward to whole pixels,
/// then clipped to `full`.
fn scaled_region(
    logical: Rectangle<i32, Logical>,
    scale: f64,
    full: Rectangle<i32, Physical>,
) -> Option<Rectangle<i32, Physical>> {
    let x0 = (logical.loc.x as f64 * scale).floor() as i32;
    let y0 = (logical.loc.y as f64 * scale).floor() as i32;
    let x1 = ((logical.loc.x + logical.size.w) as f64 * scale).ceil() as i32;
    let y1 = ((logical.loc.y + logical.size.h) as f64 * scale).ceil() as i32;
    Rectangle::<i32, Physical>::from_extremities((x0, y0), (x1, y1))
        .intersection(full)
        .filter(|r| r.size.w > 0 && r.size.h > 0)
}

fn capture(
    client: &Client,
    frame: New<ZwlrScreencopyFrameV1>,
    output: &WlOutput,
    logical: Option<Rectangle<i32, Logical>>,
    overlay_cursor: bool,
    data_init: &mut DataInit<'_, Dispatch>,
) {
    let id = REGISTRY.with_borrow_mut(|r| {
        r.next_id += 1;
        r.next_id
    });
    let frame = data_init.init(frame, FrameData { id });
    let Some(output) = Output::from_resource(output) else {
        frame.failed();
        return;
    };
    let Some(region) = physical_region(&output, logical) else {
        frame.failed();
        return;
    };
    let (client_live, total) = REGISTRY.with_borrow(|r| {
        (
            r.records
                .iter()
                .filter(|record| record.client == client.id())
                .count(),
            r.records.len(),
        )
    });
    if client_live >= MAX_CLIENT_FRAMES || total >= MAX_FRAMES {
        frame.failed();
        return;
    }
    let stride = region.size.w * 4;
    frame.buffer(
        wl_shm::Format::Xrgb8888,
        region.size.w as u32,
        region.size.h as u32,
        stride as u32,
    );
    if frame.version() >= 3 {
        frame.buffer_done();
    }
    REGISTRY.with_borrow_mut(|r| {
        r.records.push(Record {
            id,
            client: client.id(),
            output: output.name(),
            region,
            stride,
            overlay_cursor,
            submitted: false,
            pending: None,
        })
    });
}

/// The client's buffer matches the advertised shm layout.
fn valid_shm(buffer: &WlBuffer, region: Rectangle<i32, Physical>, stride: i32) -> bool {
    smithay::wayland::shm::with_buffer_contents(buffer, |_ptr, len, data| {
        matches!(
            data.format,
            wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888
        ) && data.width == region.size.w
            && data.height == region.size.h
            && data.stride == stride
            && data.offset >= 0
            && (data.offset as usize).saturating_add(stride as usize * region.size.h as usize)
                <= len
    })
    .unwrap_or(false)
}

fn submit(
    state: &mut Dispatch,
    client: &Client,
    frame: &ZwlrScreencopyFrameV1,
    id: u64,
    buffer: WlBuffer,
    with_damage: bool,
) {
    let found = REGISTRY.with_borrow(|r| {
        r.records
            .iter()
            .find(|record| record.id == id)
            .map(|record| {
                (
                    record.submitted,
                    record.region,
                    record.stride,
                    record.output.clone(),
                )
            })
    });
    let Some((submitted, region, stride, output)) = found else {
        // A frame that already failed (or finished) is not in the registry.
        frame.post_error(
            zwlr_screencopy_frame_v1::Error::AlreadyUsed,
            "screencopy frame is no longer available",
        );
        return;
    };
    if submitted {
        frame.post_error(
            zwlr_screencopy_frame_v1::Error::AlreadyUsed,
            "screencopy frame has already been used",
        );
        return;
    }
    if !valid_shm(&buffer, region, stride) {
        REGISTRY.with_borrow_mut(|r| r.records.retain(|record| record.id != id));
        frame.post_error(
            zwlr_screencopy_frame_v1::Error::InvalidBuffer,
            "buffer does not match the advertised screencopy shm layout",
        );
        return;
    }
    let immediate = !with_damage
        || REGISTRY.with_borrow(|r| {
            r.accumulators
                .iter()
                .find(|a| a.client == client.id() && a.output == output)
                .is_none_or(|a| a.rects.iter().any(|rect| rect.overlaps(region)))
        });
    REGISTRY.with_borrow_mut(|r| {
        if let Some(record) = r.records.iter_mut().find(|record| record.id == id) {
            record.submitted = true;
            record.pending = Some(Pending {
                frame: frame.clone(),
                buffer,
                with_damage,
                immediate,
            });
        }
    });
    if immediate {
        // One frame, rendered in full ([`wants_full_frame`]). Ungated: a capture
        // under exclusive pacing would otherwise wait on the pacer's next commit.
        state.redraw.request_for(RedrawReason::Capture);
        // Wake even when every old KMS flip is in flight. While paused those
        // flips cannot complete; the native capture pass ignores their schedule.
        state.redraw.wake();
    }
}

// ── Service (the backend's half) ────────────────────────────────────────────

/// Whether the next frame of `output` must be rendered IN FULL (buffer age 0):
/// a copy is owed now, and a frame the damage tracker skips or repaints
/// partially leaves a back buffer that is not the current picture.
pub fn wants_full_frame(output: &Output) -> bool {
    let name = output.name();
    REGISTRY.with_borrow(|r| {
        r.records.iter().any(|record| {
            record.output == name && record.pending.as_ref().is_some_and(|p| p.immediate)
        })
    })
}

/// Which pictures a frame of `output` that damaged `damage` must offer
/// [`service`]: one WITH the cursor and drag icon (a copy with
/// `overlay_cursor = 1`) and one WITHOUT them (`overlay_cursor = 0`, what grim
/// asks by default). Asked after the frame's own render, with its damage, so it
/// answers exactly what [`service`] will serve; a backend renders a picture only
/// when it is due, so frames that serve no copy pay nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourcesDue {
    pub cursor: bool,
    pub cursorless: bool,
}

impl SourcesDue {
    pub fn any(self) -> bool {
        self.cursor || self.cursorless
    }
}

/// See [`SourcesDue`].
pub fn sources_due(output: &Output, damage: Option<&[Rectangle<i32, Physical>]>) -> SourcesDue {
    sources_due_inner(output, damage, false)
}

fn sources_due_inner(
    output: &Output,
    damage: Option<&[Rectangle<i32, Physical>]>,
    plain_only: bool,
) -> SourcesDue {
    let name = output.name();
    let damage = damage.unwrap_or(&[]);
    REGISTRY.with_borrow(|r| {
        let mut sources = SourcesDue::default();
        for record in r.records.iter().filter(|record| record.output == name) {
            if (!plain_only || plain_copy(record)) && due(record, &r.accumulators, damage) {
                if record.overlay_cursor {
                    sources.cursor = true;
                } else {
                    sources.cursorless = true;
                }
            }
        }
        sources
    })
}

/// Whether `record`'s submitted copy is served by a frame that damaged `damage`
/// (in addition to what its client's accumulator already holds): every plain
/// copy; a damage copy when damage overlaps its region, or when its client has
/// never had a damage copy on this output (no accumulator yet).
fn due(record: &Record, accumulators: &[Accumulator], damage: &[Rectangle<i32, Physical>]) -> bool {
    match &record.pending {
        None => false,
        Some(p) if !p.with_damage => true,
        Some(_) => accumulators
            .iter()
            .find(|a| a.client == record.client && a.output == record.output)
            .is_none_or(|a| {
                a.rects
                    .iter()
                    .chain(damage)
                    .any(|rect| rect.overlaps(record.region))
            }),
    }
}

/// Whether anything at all is registered (a backend may skip [`service`] cheaply).
pub fn active() -> bool {
    REGISTRY.with_borrow(|r| !r.records.is_empty() || !r.accumulators.is_empty())
}

fn plain_copy(record: &Record) -> bool {
    record.pending.as_ref().is_some_and(|p| !p.with_damage)
}

/// Submitted plain copies eligible for a paused offscreen snapshot.
fn pending_plain(output: &Output) -> bool {
    REGISTRY.with_borrow(|r| {
        r.records
            .iter()
            .any(|record| record.output == output.name() && plain_copy(record))
    })
}

/// Fail plain snapshots on this output, preserving damage copies for a real frame.
fn fail_plain_output(output: &Output) {
    REGISTRY.with_borrow_mut(|r| {
        r.records.retain_mut(|record| {
            if record.output != output.name() || !plain_copy(record) {
                return true;
            }
            if let Some(pending) = record.pending.take() {
                pending.frame.failed();
            }
            false
        });
    });
    file::fail_output(output, "output cannot be rendered");
}

/// Fail plain snapshots, including outputs removed before a paused render ran.
fn fail_plain_pending() {
    REGISTRY.with_borrow_mut(|r| {
        r.records.retain_mut(|record| {
            if !plain_copy(record) {
                return true;
            }
            if let Some(pending) = record.pending.take() {
                pending.frame.failed();
                false
            } else {
                true
            }
        });
    });
    file::fail_all("no capture frame available");
}

/// How the backend's framebuffer is laid out for readback.
#[derive(Clone, Copy, Debug)]
pub struct Readback {
    /// The framebuffer's size in physical pixels.
    pub size: Size<i32, Physical>,
    /// Row 0 of the framebuffer is the BOTTOM of the picture: a GL default
    /// framebuffer (an EGL window surface, the nested backend). The region is
    /// mirrored vertically to read it and the rows are reversed. `false` for a
    /// top-down target (pixman images, KMS swapchain buffers).
    pub origin_bottom_left: bool,
}

/// A region of the picture, in the framebuffer's own coordinates.
fn framebuffer_region(
    region: Rectangle<i32, Physical>,
    readback: &Readback,
) -> Rectangle<i32, BufferCoords> {
    let y = if readback.origin_bottom_left {
        readback.size.h - region.loc.y - region.size.h
    } else {
        region.loc.y
    };
    Rectangle::new(
        (region.loc.x, y).into(),
        (region.size.w, region.size.h).into(),
    )
}

/// The readbacks one frame started, not yet written to their clients. Finish
/// them with [`Captures::finish`] once the frame has been handed away; dropped
/// unfinished, every frame in it is answered `failed`.
#[must_use = "finish the captures after the frame is handed away, or they fail"]
pub struct Captures<R: ExportMem> {
    output: String,
    taken: Vec<Taken<R::TextureMapping>>,
}

/// One copy whose readback has started.
struct Taken<M> {
    pending: Pending,
    region: Rectangle<i32, Physical>,
    /// The target it was read from has the picture's bottom at row 0.
    origin_bottom_left: bool,
    /// The damage to report, for a damage copy.
    reported: Option<Vec<Rectangle<i32, Physical>>>,
    /// The readback, and whether its red and blue bytes must be swapped.
    readback: Result<(M, bool), String>,
}

impl<R: ExportMem> Captures<R> {
    /// Nothing to finish.
    pub fn is_empty(&self) -> bool {
        self.taken.is_empty()
    }

    /// Map each readback, write it into the client's shm buffer and answer the
    /// frame (`flags`, `damage` for damage copies, `ready`; `failed` on error).
    /// Call after the frame has been handed away: mapping may leave a different
    /// target current on `renderer` (see the module doc).
    pub fn finish(mut self, renderer: &mut R) {
        for taken in std::mem::take(&mut self.taken) {
            if !taken.pending.frame.is_alive() {
                continue;
            }
            let written = taken.readback.and_then(|(mapping, swap_rb)| {
                write_back(
                    renderer,
                    &mapping,
                    swap_rb,
                    taken.region,
                    taken.origin_bottom_left,
                    &taken.pending.buffer,
                )
            });
            let frame = &taken.pending.frame;
            match written {
                Ok(()) => {
                    frame.flags(zwlr_screencopy_frame_v1::Flags::empty());
                    for rect in taken.reported.into_iter().flatten() {
                        frame.damage(
                            rect.loc.x as u32,
                            rect.loc.y as u32,
                            rect.size.w as u32,
                            rect.size.h as u32,
                        );
                    }
                    let (hi, lo, nsec) = now_timestamp();
                    frame.ready(hi, lo, nsec);
                }
                Err(e) => {
                    warn!("screencopy: readback of {} failed: {e}", self.output);
                    frame.failed();
                }
            }
        }
    }
}

impl<R: ExportMem> Drop for Captures<R> {
    fn drop(&mut self) {
        for taken in self.taken.drain(..) {
            if taken.pending.frame.is_alive() {
                warn!(
                    "screencopy: a capture on {} was never finished",
                    self.output
                );
                taken.pending.frame.failed();
            }
        }
    }
}

/// The target a backend reads copies from, and how it is laid out.
pub struct Source<'a, 'b, R: ExportMem> {
    pub framebuffer: &'a R::Framebuffer<'b>,
    pub readback: Readback,
}

/// Start the copies owed on `output` after rendering one of its frames; finish
/// them with [`Captures::finish`] once the frame is handed away.
///
/// `cursor` is the frame with its cursor and drag icon, `cursorless` the same
/// frame without them; [`sources_due`] says which a backend must offer. A
/// copy reads the one it asked for (`overlay_cursor`). Without it, it falls back
/// to the other (logged: the backend should have offered it), and with neither
/// it fails. The nested backend offers its window surface as `cursor` on every
/// frame, since it rendered that picture anyway.
///
/// `damage`: what this frame changed, in the output's physical coordinates
/// (top-left origin); `None` when nothing did. Called for every frame the
/// backend renders on the output, copies pending or not, so that damage copies
/// see all the damage since their client's last one.
///
/// Only `ExportMem::copy_framebuffer` runs here, which reads from a target and
/// leaves THAT target current: a backend that offers an offscreen target makes
/// its presentation target current again itself before it hands the frame away.
pub fn service<R: ExportMem>(
    renderer: &mut R,
    cursor: Option<Source<'_, '_, R>>,
    cursorless: Option<Source<'_, '_, R>>,
    output: &Output,
    damage: Option<&[Rectangle<i32, Physical>]>,
) -> Captures<R> {
    service_inner(renderer, cursor, cursorless, output, damage, false)
}

fn service_inner<R: ExportMem>(
    renderer: &mut R,
    cursor: Option<Source<'_, '_, R>>,
    cursorless: Option<Source<'_, '_, R>>,
    output: &Output,
    damage: Option<&[Rectangle<i32, Physical>]>,
    plain_only: bool,
) -> Captures<R> {
    let mut captures = Captures {
        output: output.name(),
        taken: Vec::new(),
    };
    if !active() {
        return captures;
    }
    let name = output.name();
    // Fold this frame's damage into every accumulator on the output first, so a
    // copy served by this frame reports it.
    if let Some(damage) = damage.filter(|d| !d.is_empty()) {
        REGISTRY.with_borrow_mut(|r| {
            for acc in r.accumulators.iter_mut().filter(|a| a.output == name) {
                accumulate(&mut acc.rects, damage);
            }
        });
    }
    // The copies this frame serves ([`due`], its damage now folded in). Damage
    // elsewhere on the output keeps accumulating.
    let served: Vec<Served> = REGISTRY
        .with_borrow_mut(|r| {
            let Registry {
                records,
                accumulators,
                ..
            } = r;
            let mut served = Vec::new();
            for record in records.iter_mut().filter(|record| record.output == name) {
                if (!plain_only || plain_copy(record)) && due(record, accumulators, &[]) {
                    let pending = record.pending.take().expect("due implies pending");
                    served.push((
                        record.id,
                        record.client.clone(),
                        record.region,
                        record.overlay_cursor,
                        pending,
                    ));
                }
            }
            served
        });
    for (id, client, region, overlay_cursor, pending) in served {
        REGISTRY.with_borrow_mut(|r| r.records.retain(|record| record.id != id));
        if !pending.frame.is_alive() {
            continue;
        }
        let reported = if pending.with_damage {
            Some(take_damage(&client, &name, region))
        } else {
            None
        };
        // Which picture to read: the one asked for, else the other one. Chosen
        // as a flag and read per branch, because the two `Source`s carry
        // distinct (invariant) framebuffer lifetimes and cannot share a binding.
        let use_cursor = match (overlay_cursor, cursor.is_some(), cursorless.is_some()) {
            (true, true, _) => true,
            (false, _, true) => false,
            (_, false, false) => {
                warn!("screencopy: a copy on {name} failed: the backend offered no picture");
                pending.frame.failed();
                continue;
            }
            (wanted, has_cursor, _) => {
                warn!(
                    "screencopy: a copy on {name} with overlay_cursor = {wanted} read the other picture (not offered)"
                );
                has_cursor
            }
        };
        let (started, origin_bottom_left) = match (use_cursor, &cursor, &cursorless) {
            (true, Some(source), _) => (
                start_readback(renderer, source.framebuffer, region, &source.readback),
                source.readback.origin_bottom_left,
            ),
            (false, _, Some(source)) => (
                start_readback(renderer, source.framebuffer, region, &source.readback),
                source.readback.origin_bottom_left,
            ),
            _ => unreachable!("use_cursor names a picture that was offered"),
        };
        captures.taken.push(Taken {
            pending,
            region,
            origin_bottom_left,
            reported,
            readback: started,
        });
    }
    captures
}

/// Render `elements` (front to back, as a frame takes them) that pass `keep`
/// into a new offscreen texture of `size`, in full: a fresh tracker, age 0,
/// untransformed, so the texture is TOP-DOWN (`Readback::origin_bottom_left:
/// false`). A backend uses it for the pictures [`sources_due`] asks for that it
/// does not already have: the frame without its pointer elements on every
/// backend when the pointer is baked into its presentation buffer. Native
/// otherwise copies its frame result into an [`offscreen_texture`].
///
/// Rendering leaves the texture current on the context: a backend that
/// presents to an EGL window surface makes that current again before its swap.
pub fn render_offscreen<E>(
    renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    elements: &[E],
    keep: impl Fn(&E) -> bool,
    size: Size<i32, Physical>,
    scale: f64,
) -> Result<smithay::backend::renderer::gles::GlesTexture, String>
where
    E: smithay::backend::renderer::element::RenderElement<
            smithay::backend::renderer::gles::GlesRenderer,
        >,
{
    use smithay::backend::renderer::damage::OutputDamageTracker;
    use smithay::backend::renderer::Bind;
    let mut texture = offscreen_texture(renderer, size)?;
    {
        let mut target = renderer
            .bind(&mut texture)
            .map_err(|err| format!("bind the offscreen texture: {err}"))?;
        let kept: Vec<&E> = elements.iter().filter(|element| keep(*element)).collect();
        OutputDamageTracker::new(size, scale, smithay::utils::Transform::Normal)
            .render_output(renderer, &mut target, 0, &kept, [0.0, 0.0, 0.0, 1.0])
            .map_err(|err| format!("offscreen render: {err:?}"))?;
    }
    Ok(texture)
}

/// Allocate a readable output target without replaying its elements. Native
/// can populate this from its KMS frame result, including hardware planes.
pub fn offscreen_texture(
    renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    size: Size<i32, Physical>,
) -> Result<smithay::backend::renderer::gles::GlesTexture, String> {
    use smithay::backend::renderer::Offscreen;
    renderer
        .create_buffer(
            Fourcc::Abgr8888,
            Size::<i32, BufferCoords>::from((size.w, size.h)),
        )
        .map_err(|err| format!("offscreen texture: {err}"))
}

/// One `frame_damage` tracker: output name, the size and scale it was made
/// for, and the tracker.
type Tracker = (
    String,
    Size<i32, Physical>,
    f64,
    smithay::backend::renderer::damage::OutputDamageTracker,
);

thread_local! {
    // `frame_damage`'s trackers, one per output.
    static TRACKERS: RefCell<Vec<Tracker>> = const { RefCell::new(Vec::new()) };
}

/// This frame's damage on `output` (`elements` front to back, at `size` and
/// `scale`, untransformed) for a backend whose own render does not hand its
/// damage out (native: `DrmCompositor` keeps it). Diffs against the previous
/// call on the output (age 1), without rendering anything.
///
/// Tracks only while screencopy is [`active`]: otherwise its tracker is dropped
/// and `None` returned, so an idle desktop does no work here, and the first
/// frame after it becomes active reports the whole output (more damage, never
/// less).
pub fn frame_damage<E: smithay::backend::renderer::element::Element>(
    output: &Output,
    size: Size<i32, Physical>,
    scale: f64,
    elements: &[E],
) -> Option<Vec<Rectangle<i32, Physical>>> {
    let name = output.name();
    TRACKERS.with_borrow_mut(|trackers| {
        if !active() {
            trackers.retain(|(n, ..)| *n != name);
            return None;
        }
        let at = match trackers.iter().position(|(n, ..)| *n == name) {
            Some(at) if trackers[at].1 == size && trackers[at].2 == scale => at,
            found => {
                if let Some(at) = found {
                    trackers.remove(at);
                }
                trackers.push((
                    name,
                    size,
                    scale,
                    smithay::backend::renderer::damage::OutputDamageTracker::new(
                        size,
                        scale,
                        smithay::utils::Transform::Normal,
                    ),
                ));
                trackers.len() - 1
            }
        };
        match trackers[at].3.damage_output(1, elements) {
            Ok((damage, _)) => damage.filter(|d| !d.is_empty()).cloned(),
            Err(err) => {
                warn!("screencopy: no frame damage ({err:?}); reporting the whole output");
                Some(vec![Rectangle::from_size(size)])
            }
        }
    })
}

/// The damage to report for a damage copy by `client` on `output`, clipped to
/// `region` and made region-local, then reset. A first damage copy (no
/// accumulator yet) reports the whole region and starts one.
fn take_damage(
    client: &ClientId,
    output: &str,
    region: Rectangle<i32, Physical>,
) -> Vec<Rectangle<i32, Physical>> {
    REGISTRY.with_borrow_mut(|r| {
        let rects = match r
            .accumulators
            .iter_mut()
            .find(|a| &a.client == client && a.output == output)
        {
            Some(acc) => std::mem::take(&mut acc.rects),
            None => {
                if r.accumulators.len() >= MAX_ACCUMULATORS {
                    r.accumulators.remove(0);
                }
                r.accumulators.push(Accumulator {
                    client: client.clone(),
                    output: output.to_string(),
                    rects: Vec::new(),
                });
                vec![region]
            }
        };
        clip_damage(&rects, region)
    })
}

/// `rects` clipped to `region`, in region-local coordinates; the whole region
/// when nothing intersects it (a copy was served, so something is reported).
fn clip_damage(
    rects: &[Rectangle<i32, Physical>],
    region: Rectangle<i32, Physical>,
) -> Vec<Rectangle<i32, Physical>> {
    let clipped: Vec<_> = rects
        .iter()
        .filter_map(|rect| rect.intersection(region))
        .filter(|rect| rect.size.w > 0 && rect.size.h > 0)
        .map(|rect| {
            Rectangle::new(
                (rect.loc.x - region.loc.x, rect.loc.y - region.loc.y).into(),
                rect.size,
            )
        })
        .collect();
    if clipped.is_empty() {
        vec![Rectangle::from_size(region.size)]
    } else {
        clipped
    }
}

/// Add `damage` to `acc`, merging into one bounding box beyond
/// [`MAX_ACCUMULATED_RECTS`] (more damage reported, never less).
fn accumulate(acc: &mut Vec<Rectangle<i32, Physical>>, damage: &[Rectangle<i32, Physical>]) {
    acc.extend_from_slice(damage);
    if acc.len() > MAX_ACCUMULATED_RECTS {
        let merged = acc.iter().skip(1).fold(acc[0], |a, b| a.merge(*b));
        acc.clear();
        acc.push(merged);
    }
}

/// Start reading `region` of the framebuffer back; the mapping, and whether its
/// red and blue bytes must be swapped.
fn start_readback<R: ExportMem>(
    renderer: &mut R,
    framebuffer: &R::Framebuffer<'_>,
    region: Rectangle<i32, Physical>,
    readback: &Readback,
) -> Result<(R::TextureMapping, bool), String> {
    let source = framebuffer_region(region, readback);
    // XRGB8888 is the shm layout (B, G, R, X in memory). Where the renderer
    // cannot read that order, read RGBA and swap the red and blue bytes.
    match renderer.copy_framebuffer(framebuffer, source, Fourcc::Xrgb8888) {
        Ok(mapping) => Ok((mapping, false)),
        Err(_) => renderer
            .copy_framebuffer(framebuffer, source, Fourcc::Abgr8888)
            .map(|mapping| (mapping, true))
            .map_err(|e| format!("copy_framebuffer: {e}")),
    }
}

/// Map a started readback of `region` and write it into the client's shm
/// `buffer`, top row first.
fn write_back<R: ExportMem>(
    renderer: &mut R,
    mapping: &R::TextureMapping,
    swap_rb: bool,
    region: Rectangle<i32, Physical>,
    origin_bottom_left: bool,
    buffer: &WlBuffer,
) -> Result<(), String> {
    let bytes = renderer
        .map_texture(mapping)
        .map_err(|e| format!("map_texture: {e}"))?;
    let width = region.size.w as usize;
    let height = region.size.h as usize;
    let row = width * 4;
    if bytes.len() < row * height {
        return Err(format!(
            "readback returned {} bytes, {} expected",
            bytes.len(),
            row * height
        ));
    }
    smithay::wayland::shm::with_buffer_contents_mut(buffer, |ptr, len, data| {
        for y in 0..height {
            let src = source_row(y, height, origin_bottom_left);
            let from = &bytes[src * row..src * row + row];
            let at = data.offset as usize + y * data.stride as usize;
            if at + row > len {
                return Err("shm buffer shorter than its layout".to_string());
            }
            // SAFETY: `at + row <= len`, and `ptr` covers `len` bytes of the pool,
            // which wayland-server keeps mapped for the closure's duration.
            let to = unsafe { std::slice::from_raw_parts_mut(ptr.add(at), row) };
            to.copy_from_slice(from);
            if swap_rb {
                for px in to.chunks_exact_mut(4) {
                    px.swap(0, 2);
                }
            }
        }
        Ok(())
    })
    .map_err(|e| format!("shm access: {e:?}"))?
}

/// Which readback row holds picture row `y`.
fn source_row(y: usize, height: usize, origin_bottom_left: bool) -> usize {
    if origin_bottom_left {
        height - 1 - y
    } else {
        y
    }
}

/// CLOCK_MONOTONIC now, split as `ready` wants it.
fn now_timestamp() -> (u32, u32, u32) {
    let us = ledger::frame_trace::monotonic_us();
    let secs = us / 1_000_000;
    let nsec = (us % 1_000_000) * 1_000;
    ((secs >> 32) as u32, secs as u32, nsec as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn logical_regions_scale_outward_and_clip() {
        let full = rect(0, 0, 1920, 1080);
        let r = Rectangle::<i32, Logical>::new((10, 10).into(), (100, 50).into());
        assert_eq!(scaled_region(r, 1.0, full), Some(rect(10, 10, 100, 50)));
        // 1.5: 15..165 x 15..90
        assert_eq!(scaled_region(r, 1.5, full), Some(rect(15, 15, 150, 75)));
        let outside = Rectangle::<i32, Logical>::new((5000, 0).into(), (10, 10).into());
        assert_eq!(scaled_region(outside, 1.0, full), None);
        let straddle = Rectangle::<i32, Logical>::new((1900, 1070).into(), (100, 100).into());
        assert_eq!(
            scaled_region(straddle, 1.0, full),
            Some(rect(1900, 1070, 20, 10))
        );
    }

    #[test]
    fn a_bottom_left_framebuffer_reads_the_mirrored_region_bottom_up() {
        let rb = Readback {
            size: (100, 80).into(),
            origin_bottom_left: true,
        };
        let region = rect(10, 5, 20, 30);
        assert_eq!(
            framebuffer_region(region, &rb),
            Rectangle::new((10, 45).into(), (20, 30).into())
        );
        assert_eq!(
            source_row(0, 30, true),
            29,
            "picture top is the last readback row"
        );
        let top = Readback {
            origin_bottom_left: false,
            ..rb
        };
        assert_eq!(
            framebuffer_region(region, &top),
            Rectangle::new((10, 5).into(), (20, 30).into())
        );
        assert_eq!(source_row(0, 30, false), 0);
    }

    #[test]
    fn damage_is_clipped_region_local_and_never_empty() {
        let region = rect(100, 100, 50, 50);
        assert_eq!(
            clip_damage(&[rect(90, 120, 20, 10)], region),
            [rect(0, 20, 10, 10)]
        );
        assert_eq!(
            clip_damage(&[rect(0, 0, 10, 10)], region),
            [rect(0, 0, 50, 50)],
            "nothing inside: the whole region"
        );
    }

    #[test]
    fn accumulation_merges_past_the_cap() {
        let mut acc = Vec::new();
        let many: Vec<_> = (0..MAX_ACCUMULATED_RECTS as i32 + 1)
            .map(|i| rect(i, i, 1, 1))
            .collect();
        accumulate(&mut acc, &many);
        assert_eq!(acc.len(), 1);
        assert_eq!(
            acc[0],
            rect(
                0,
                0,
                MAX_ACCUMULATED_RECTS as i32 + 1,
                MAX_ACCUMULATED_RECTS as i32 + 1
            )
        );
    }
}
