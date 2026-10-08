// The projections from the engine's state (`snapshot`, `read_snapshot`,
// `project_*`) stay with the engine. The full-tree cache is a std `OnceLock`,
// so the read verbs are synchronous: the transport runs them off the
// compositor thread.

//! The `comp.*` props tree (`CompSnapshot`) and the read verbs over it:
//! `comp.info`, `comp.windows.list`, `comp.props.get`, `comp.props.list`
//! and `comp.props.describe`.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, OnceLock},
};

use serde::Serialize;
use serde_json::{Value, json};

use ledger::presentation::SourcePresentationLeaves;
use ledger::presentation_stats::{OutputStats, PresentationLeaves};

use crate::observation::{CornerConfig, HOLDER_PLANE_AVAILABLE, SetValidationError};
use crate::prop_path::PropPath;
use crate::reply::{ControlReply, MAX_REPLY_BODY_BYTES, error, too_large};

pub const BROKER_RETRYING: u8 = 0;
pub const BROKER_CONNECTED: u8 = 1;

/// `CLOCK_MONOTONIC` on Linux: the `outputs.<key>.presentation.clock_id`
/// value (`wp_presentation.clock_id`).
pub const CLOCK_MONOTONIC_ID: u32 = 1;

/// The served occlusion counters (`occlusion.counters.*`, volatile).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct OcclusionCounters {
    pub withheld_opportunities: u64,
    pub resumes: u64,
    pub recomputes: u64,
    pub conservative_fallbacks: u64,
}

/// The per-surface occlusion leaves (flattened into `surfaces.*` and
/// `windows.*` rows).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OcclusionProps {
    pub occluded: bool,
    pub occlusion_reason: &'static str,
    pub occlusion_revision: u64,
}

impl Default for OcclusionProps {
    fn default() -> Self {
        Self {
            occluded: false,
            occlusion_reason: "unknown",
            occlusion_revision: 0,
        }
    }
}

/// One refused linux-dmabuf import, as `dmabuf.failures[]` serves it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DmabufFailureRecord {
    /// DRM fourcc as its four characters (`"AR24"`); a non-printable byte
    /// reads `?`.
    pub format: String,
    /// DRM format modifier, `0x` + 16 hex digits.
    pub modifier: String,
    pub reason: &'static str,
    /// The refusing check's own message.
    pub detail: String,
    /// CLOCK_MONOTONIC µs when the import was refused.
    pub at_us: u64,
}

/// A consistent copy of the dmabuf import ledger for one read.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DmabufLedgerSnapshot {
    pub accepted: u64,
    pub failed: u64,
    pub failures: Vec<DmabufFailureRecord>,
}

/// The single-flight cache of one snapshot's full-tree serialisation.
/// Cloning a snapshot does not clone its cache: a clone may be edited.
#[derive(Debug, Default)]
pub struct FullTreeCache(OnceLock<SerialisedReply>);

impl Clone for FullTreeCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl FullTreeCache {
    pub fn is_filled(&self) -> bool {
        self.0.get().is_some()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CompSnapshot {
    pub occlusion: OcclusionSnapshot,
    pub info: InfoSnapshot,
    pub outputs: BTreeMap<String, OutputSnapshot>,
    pub surfaces: BTreeMap<String, SurfaceSnapshot>,
    pub windows: BTreeMap<String, WindowSnapshot>,
    pub workspaces: WorkspacesSnapshot,
    pub sources: BTreeMap<String, SourceSnapshot>,
    pub stack: Vec<u64>,
    pub focus: FocusSnapshot,
    pub decoration: DecorationSnapshot,
    pub bindings: BindingsSnapshot,
    pub input: InputSnapshot,
    #[cfg(feature = "xwayland")]
    pub xwayland: XwaylandSnapshot,
    /// Observed linux-dmabuf import outcomes (volatile; filled only in read
    /// snapshots, so the diff snapshot never sees them change).
    pub dmabuf: DmabufLedgerSnapshot,
    pub port: PortSnapshot,
    #[serde(skip)]
    pub full_tree: FullTreeCache,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct OcclusionSnapshot {
    pub counters: OcclusionCounters,
}

#[derive(Clone, Debug)]
pub struct SerialisedReply {
    body: Arc<str>,
    bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InfoSnapshot {
    pub service: Arc<str>,
    pub version: Arc<str>,
    pub backend: &'static str,
    pub engine: &'static str,
    pub instance: Arc<str>,
    pub explicit_sync_advertised: bool,
    pub explicit_sync_healthy: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutputSnapshot {
    pub name: String,
    pub default: bool,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub refresh_mhz: u32,
    pub usable: RectSnapshot,
    /// Volatile; filled only in read snapshots (never in diffed rows).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation: Option<OutputPresentationSnapshot>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RectSnapshot {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SurfaceSnapshot {
    #[serde(flatten)]
    pub occlusion: OcclusionProps,
    pub id: u64,
    pub role: &'static str,
    pub mapped: bool,
    pub visible: bool,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub band: &'static str,
    pub sequence: u64,
    pub tree_index: u32,
    pub parent: Option<u64>,
    pub output: Option<String>,
    pub title: Option<Arc<str>>,
    pub app_id: Option<Arc<str>>,
    pub focused: bool,
    pub activated: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    pub minimized: bool,
    /// The 1-based workspace of a mapped managed toplevel (X11 included —
    /// the one place an X11 window's workspace is legible, D11); null for
    /// every other role and before the first map.
    pub workspace: Option<u32>,
    pub decoration: Option<&'static str>,
    pub layer: Option<LayerSnapshot>,
    pub foreign_id: Option<String>,
    pub generation: u64,
    /// Window-only values carried to `project_window_row`; not part of the
    /// `surfaces.*` tree.
    #[serde(skip)]
    pub window: WindowExtras,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowExtras {
    pub tiled: bool,
    pub requested_tiled: bool,
    pub native_requested_tiled: bool,
    pub tile_pending_reason: Option<&'static str>,
    pub configure_pending: bool,
    pub window_x: f32,
    pub window_y: f32,
    pub window_width: f32,
    pub window_height: f32,
    pub pid: Option<u64>,
    pub workspace: u32,
}

/// `workspaces.*`: the count, the default output's current workspace
/// (`current`), one `o_<slug>.current` per output (the same keys as
/// `outputs.*`) and the per-workspace window counts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkspacesSnapshot {
    pub count: u32,
    pub current: u32,
    #[serde(flatten)]
    pub outputs: BTreeMap<String, OutputWorkspaceSnapshot>,
    pub list: Vec<WorkspaceRowSnapshot>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct OutputWorkspaceSnapshot {
    pub current: u32,
}

/// One `workspaces.list` entry: the 1-based index and how many mapped
/// managed toplevels are on it (X11 windows included: every row with a
/// non-null `surfaces.s<id>.workspace`, not only the `windows.*` rows).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct WorkspaceRowSnapshot {
    pub index: u32,
    pub windows: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LayerSnapshot {
    pub stratum: &'static str,
    pub interactivity: &'static str,
    pub exclusive_zone: i32,
    pub binding: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WindowSnapshot {
    pub tiled: bool,
    pub requested_tiled: bool,
    pub native_requested_tiled: bool,
    pub tile_pending_reason: Option<&'static str>,
    pub configure_pending: bool,
    #[serde(flatten)]
    pub occlusion: OcclusionProps,
    pub id: u64,
    pub foreign_id: Option<String>,
    pub title: Option<Arc<str>>,
    pub app_id: Option<Arc<str>>,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub focused: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    pub minimized: bool,
    pub output: Option<String>,
    pub band: &'static str,
    pub generation: u64,
    pub window_x: f32,
    pub window_y: f32,
    pub window_width: f32,
    pub window_height: f32,
    pub visible: bool,
    pub pid: Option<u64>,
    /// The window's 1-based workspace (writable; a move never switches).
    pub workspace: u32,
    /// Volatile; filled only in read snapshots (never in diffed rows).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation: Option<PresentationLeaves>,
}

/// `outputs.o_<slug>.presentation.*` (volatile).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OutputPresentationSnapshot {
    pub clock_id: u32,
    /// Kind flag names of the newest frame; null before the first frame.
    pub flags: Option<Vec<&'static str>>,
    pub flags_mask: Option<u32>,
    pub refresh_us: Option<u64>,
    pub frames: u64,
    pub interval_p50_us: Option<u64>,
    pub interval_p99_us: Option<u64>,
    pub since_us: u64,
}

/// `sources.<id>` (volatile, like everything a source reports).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceSnapshot {
    pub output: Option<String>,
    pub registered_at_us: u64,
    pub revision: u64,
    pub registration: u64,
    pub presentation: SourcePresentationLeaves,
}

/// Select inside a small volatile object through its serialised form.
fn select_serialised<T: Serialize>(value: &T, path: &[&str]) -> Option<Value> {
    let mut node = serialise_selected(value)?;
    for segment in path {
        node = node.as_object_mut()?.remove(*segment)?;
    }
    Some(node)
}

fn serialised_node_kind<T: Serialize>(value: &T, path: &[&str]) -> Option<SnapshotNodeKind> {
    select_serialised(value, path).map(|node| {
        if node.is_object() {
            SnapshotNodeKind::Object
        } else {
            SnapshotNodeKind::Leaf
        }
    })
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FocusSnapshot {
    pub keyboard: Option<u64>,
    pub exclusive_latch: Option<u64>,
    pub pointer: Option<u64>,
    pub pointer_grab: &'static str,
    pub session_lock: &'static str,
    pub window: FocusWindowSnapshot,
}

/// `{id, generation}` of the focused window row, both null when no window
/// has keyboard focus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct FocusWindowSnapshot {
    pub id: Option<u64>,
    pub generation: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DecorationSnapshot {
    pub enabled: bool,
    pub style: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BindingsSnapshot {
    pub enabled: bool,
    pub profile: &'static str,
    pub table: Vec<BindingRowSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BindingRowSnapshot {
    pub chord: String,
    pub action: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InputSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seats: Option<BTreeMap<&'static str, SeatSnapshot>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_origin: Option<&'static str>,
    pub corners: CornersSnapshot,
    /// Nested backend only: whether host pointer/key input reaches the seat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<HostInputSnapshot>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct HostInputSnapshot {
    pub passthrough: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SeatSnapshot {
    /// The advertised `wl_seat` name (`surfaces::SeatKind::seat_name`).
    pub name: &'static str,
    pub keyboard_focus: Option<SeatFocusSnapshot>,
    pub pointer_focus: Option<SeatFocusSnapshot>,
    pub pointer: Option<SeatPointerSnapshot>,
    pub last_input_us: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SeatFocusSnapshot {
    pub id: u64,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SeatPointerSnapshot {
    pub output: String,
    pub x: f64,
    pub y: f64,
}

/// The XWayland runtime switch as a props subtree: `xwayland.enabled` is
/// the CONFIGURED value (startup-read; a set persists for the next
/// compositor startup — not whether a generation is currently running,
/// which the lifecycle owns), and `xwayland.persist_path` is the resolved
/// per-socket file that value persists to — read-only, surfaced because
/// the path depends on the MixOS config root and the socket name, and an
/// operator must be able to SEE which file governs the next startup
/// rather than deduce it.
#[cfg(feature = "xwayland")]
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct XwaylandSnapshot {
    pub enabled: bool,
    pub persist_path: Arc<str>,
    /// The X display this compositor's Xwayland serves (`:N`), null until
    /// the generation is ready (XWM started, descriptor published) and
    /// again after it goes down. Read it rather than the descriptor file
    /// when the caller already speaks Bus.
    pub display: Option<Arc<str>>,
    /// Where the running server is in its life:
    /// `off`, `starting`, `ready`, `retrying` (the one restart is armed) or
    /// `failed` (down until the compositor restarts).
    pub state: &'static str,
    /// X server deaths this session, startup crashes included.
    pub failures: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct CornersSnapshot {
    pub holders: bool,
    pub enabled: bool,
    pub deadzone_px: f64,
    pub dwell_ms: u64,
    pub velocity_max_px_s: f64,
    pub affordance: bool,
    pub discovery: bool,
    /// Volatile, read snapshots only: per edge, the panel layers the
    /// compositor is hiding and excluding from input itself because a
    /// conceal went unapplied past its grace (a stalled shell).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enforced: Option<EdgeCounts>,
    /// Volatile, read snapshots only: per edge, the explicit holds
    /// (`comp.panel.hold` acquisitions) comp currently records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held: Option<EdgeCounts>,
}

/// One count per panel edge, summed over outputs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct EdgeCounts {
    pub top: u64,
    pub bottom: u64,
    pub left: u64,
    pub right: u64,
}

impl EdgeCounts {
    pub fn edge_mut(&mut self, edge: &str) -> Option<&mut u64> {
        match edge {
            "top" => Some(&mut self.top),
            "bottom" => Some(&mut self.bottom),
            "left" => Some(&mut self.left),
            "right" => Some(&mut self.right),
            _ => None,
        }
    }
}

impl From<CornerConfig> for CornersSnapshot {
    fn from(config: CornerConfig) -> Self {
        Self {
            holders: HOLDER_PLANE_AVAILABLE,
            enabled: config.enabled,
            deadzone_px: config.deadzone_px,
            dwell_ms: config.dwell_ms,
            velocity_max_px_s: config.velocity_max_px_s,
            affordance: config.affordance,
            discovery: config.discovery,
            enforced: None,
            held: None,
        }
    }
}

impl CornersSnapshot {
    fn select(&self, path: &[&str]) -> Option<Value> {
        select_serialised(self, path)
    }

    fn node_kind(&self, path: &[&str]) -> Option<SnapshotNodeKind> {
        serialised_node_kind(self, path)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PortSnapshot {
    pub level: &'static str,
    pub event_seq: u64,
    pub lost_count: u64,
    pub queue_depth: usize,
    pub reply_timeouts: u64,
    pub publish_timeouts: u64,
    pub slug_collisions: u64,
    pub broker: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SnapshotNodeKind {
    Leaf,
    Object,
}

fn serialise_selected<T: Serialize>(value: &T) -> Option<Value> {
    serde_json::to_value(value).ok()
}

impl CompSnapshot {
    fn select(&self, path: &[&str]) -> Option<Value> {
        let [head, tail @ ..] = path else {
            return None;
        };
        match *head {
            "occlusion" => select_serialised(&self.occlusion, tail),
            "info" => self.info.select(tail),
            "outputs" => select_map(&self.outputs, tail, OutputSnapshot::select),
            "surfaces" => select_map(&self.surfaces, tail, SurfaceSnapshot::select),
            "windows" => select_map(&self.windows, tail, WindowSnapshot::select),
            "workspaces" => self.workspaces.select(tail),
            "sources" => select_map(&self.sources, tail, select_serialised::<SourceSnapshot>),
            "stack" if tail.is_empty() => serialise_selected(&self.stack),
            "focus" => self.focus.select(tail),
            "decoration" => self.decoration.select(tail),
            "bindings" => self.bindings.select(tail),
            "input" => self.input.select(tail),
            #[cfg(feature = "xwayland")]
            "xwayland" => self.xwayland.select(tail),
            "dmabuf" => self.dmabuf.select(tail),
            "port" => self.port.select(tail),
            _ => None,
        }
    }

    fn node_kind(&self, path: &[&str]) -> Option<SnapshotNodeKind> {
        let [head, tail @ ..] = path else {
            return None;
        };
        match *head {
            "occlusion" => serialised_node_kind(&self.occlusion, tail),
            "info" => self.info.node_kind(tail),
            "outputs" => map_node_kind(&self.outputs, tail, OutputSnapshot::node_kind),
            "surfaces" => map_node_kind(&self.surfaces, tail, SurfaceSnapshot::node_kind),
            "windows" => map_node_kind(&self.windows, tail, WindowSnapshot::node_kind),
            "workspaces" => self.workspaces.node_kind(tail),
            "sources" => map_node_kind(&self.sources, tail, serialised_node_kind::<SourceSnapshot>),
            "stack" if tail.is_empty() => Some(SnapshotNodeKind::Leaf),
            "focus" => self.focus.node_kind(tail),
            "decoration" => self.decoration.node_kind(tail),
            "bindings" => self.bindings.node_kind(tail),
            "input" => self.input.node_kind(tail),
            #[cfg(feature = "xwayland")]
            "xwayland" => self.xwayland.node_kind(tail),
            "dmabuf" => self.dmabuf.node_kind(tail),
            "port" => self.port.node_kind(tail),
            _ => None,
        }
    }

    fn leaf_paths(&self) -> Vec<PropPath> {
        let mut paths = Vec::new();
        for descriptor in DESCRIPTORS {
            for candidate in self.expand_pattern(descriptor.pattern) {
                let Ok(path) = PropPath::new(candidate) else {
                    continue;
                };
                let segments = path.segments().collect::<Vec<_>>();
                if self.node_kind(&segments) == Some(SnapshotNodeKind::Leaf) {
                    paths.push(path);
                }
            }
        }
        paths.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        paths
    }

    fn expand_pattern(&self, pattern: &[PatternSegment]) -> Vec<String> {
        let mut paths = vec![String::new()];
        for segment in pattern {
            match segment {
                PatternSegment::Literal(segment) => append_segments(&mut paths, [*segment]),
                PatternSegment::OutputKey => {
                    append_segments(&mut paths, self.outputs.keys().map(String::as_str));
                }
                PatternSegment::SourceKey => {
                    append_segments(&mut paths, self.sources.keys().map(String::as_str));
                }
                PatternSegment::SeatKey => append_segments(&mut paths, ["human", "agent"]),
                PatternSegment::SurfaceKey => match pattern.first() {
                    Some(PatternSegment::Literal("surfaces")) => {
                        append_segments(&mut paths, self.surfaces.keys().map(String::as_str))
                    }
                    Some(PatternSegment::Literal("windows")) => {
                        append_segments(&mut paths, self.windows.keys().map(String::as_str));
                    }
                    _ => return Vec::new(),
                },
            }
        }
        paths
    }
}

fn append_segments<'a>(
    paths: &mut Vec<String>,
    segments: impl IntoIterator<Item = &'a str> + Clone,
) {
    let existing = std::mem::take(paths);
    for path in existing {
        for segment in segments.clone() {
            paths.push(if path.is_empty() {
                segment.to_string()
            } else {
                format!("{path}.{segment}")
            });
        }
    }
}

fn select_map<T>(
    values: &BTreeMap<String, T>,
    path: &[&str],
    select: fn(&T, &[&str]) -> Option<Value>,
) -> Option<Value>
where
    T: Serialize,
{
    let Some((key, tail)) = path.split_first() else {
        return serialise_selected(values);
    };
    select(values.get(*key)?, tail)
}

fn map_node_kind<T>(
    values: &BTreeMap<String, T>,
    path: &[&str],
    node_kind: fn(&T, &[&str]) -> Option<SnapshotNodeKind>,
) -> Option<SnapshotNodeKind> {
    let Some((key, tail)) = path.split_first() else {
        return Some(SnapshotNodeKind::Object);
    };
    node_kind(values.get(*key)?, tail)
}

macro_rules! flat_snapshot {
    ($ty:ty, $($field:ident),+ $(,)?) => {
        impl $ty {
            fn select(&self, path: &[&str]) -> Option<Value> {
                match path {
                    [] => serialise_selected(self),
                    $([stringify!($field)] => serialise_selected(&self.$field),)+
                    _ => None,
                }
            }

            fn node_kind(&self, path: &[&str]) -> Option<SnapshotNodeKind> {
                match path {
                    [] => Some(SnapshotNodeKind::Object),
                    $([stringify!($field)] => Some(SnapshotNodeKind::Leaf),)+
                    _ => None,
                }
            }
        }
    };
}

flat_snapshot!(
    InfoSnapshot,
    service,
    version,
    backend,
    engine,
    instance,
    explicit_sync_advertised,
    explicit_sync_healthy,
);
#[cfg(feature = "xwayland")]
flat_snapshot!(
    XwaylandSnapshot,
    enabled,
    persist_path,
    display,
    state,
    failures
);
flat_snapshot!(DmabufLedgerSnapshot, accepted, failed, failures);
flat_snapshot!(OutputWorkspaceSnapshot, current);

impl WorkspacesSnapshot {
    fn select(&self, path: &[&str]) -> Option<Value> {
        match path {
            [] => serialise_selected(self),
            ["count"] => serialise_selected(&self.count),
            ["current"] => serialise_selected(&self.current),
            ["list"] => serialise_selected(&self.list),
            _ => select_map(&self.outputs, path, OutputWorkspaceSnapshot::select),
        }
    }

    fn node_kind(&self, path: &[&str]) -> Option<SnapshotNodeKind> {
        match path {
            [] => Some(SnapshotNodeKind::Object),
            ["count" | "current" | "list"] => Some(SnapshotNodeKind::Leaf),
            _ => map_node_kind(&self.outputs, path, OutputWorkspaceSnapshot::node_kind),
        }
    }
}

macro_rules! window_snapshot {
    ($($field:ident),+ $(,)?) => {
        impl WindowSnapshot {
            fn select(&self, path: &[&str]) -> Option<Value> {
                match path {
                    [] => serialise_selected(self),
                    $([stringify!($field)] => serialise_selected(&self.$field),)+
                    ["presentation", tail @ ..] => {
                        select_serialised(self.presentation.as_ref()?, tail)
                    }
                    _ => select_serialised(&self.occlusion, path),
                }
            }

            fn node_kind(&self, path: &[&str]) -> Option<SnapshotNodeKind> {
                match path {
                    [] => Some(SnapshotNodeKind::Object),
                    $([stringify!($field)] => Some(SnapshotNodeKind::Leaf),)+
                    ["presentation", tail @ ..] => {
                        serialised_node_kind(self.presentation.as_ref()?, tail)
                    }
                    _ => serialised_node_kind(&self.occlusion, path),
                }
            }
        }
    };
}

window_snapshot!(
    tiled,
    requested_tiled,
    native_requested_tiled,
    tile_pending_reason,
    configure_pending,
    id,
    foreign_id,
    title,
    app_id,
    x,
    y,
    width,
    height,
    focused,
    maximized,
    fullscreen,
    minimized,
    output,
    band,
    generation,
    window_x,
    window_y,
    window_width,
    window_height,
    visible,
    pid,
    workspace,
);
flat_snapshot!(FocusWindowSnapshot, id, generation);

impl FocusSnapshot {
    fn select(&self, path: &[&str]) -> Option<Value> {
        match path {
            [] => serialise_selected(self),
            ["keyboard"] => serialise_selected(&self.keyboard),
            ["exclusive_latch"] => serialise_selected(&self.exclusive_latch),
            ["pointer"] => serialise_selected(&self.pointer),
            ["pointer_grab"] => serialise_selected(&self.pointer_grab),
            ["session_lock"] => serialise_selected(&self.session_lock),
            ["window", tail @ ..] => self.window.select(tail),
            _ => None,
        }
    }

    fn node_kind(&self, path: &[&str]) -> Option<SnapshotNodeKind> {
        match path {
            [] | ["window"] => Some(SnapshotNodeKind::Object),
            ["keyboard" | "exclusive_latch" | "pointer" | "pointer_grab" | "session_lock"] => {
                Some(SnapshotNodeKind::Leaf)
            }
            ["window", tail @ ..] => self.window.node_kind(tail),
            _ => None,
        }
    }
}
flat_snapshot!(DecorationSnapshot, enabled, style);
flat_snapshot!(BindingsSnapshot, enabled, profile, table);
flat_snapshot!(
    PortSnapshot,
    level,
    event_seq,
    lost_count,
    queue_depth,
    reply_timeouts,
    publish_timeouts,
    slug_collisions,
    broker,
);
flat_snapshot!(RectSnapshot, x, y, width, height);
flat_snapshot!(
    LayerSnapshot,
    stratum,
    interactivity,
    exclusive_zone,
    binding,
);

impl OutputSnapshot {
    fn select(&self, path: &[&str]) -> Option<Value> {
        match path {
            [] => serialise_selected(self),
            ["name"] => serialise_selected(&self.name),
            ["default"] => serialise_selected(&self.default),
            ["x"] => serialise_selected(&self.x),
            ["y"] => serialise_selected(&self.y),
            ["width"] => serialise_selected(&self.width),
            ["height"] => serialise_selected(&self.height),
            ["scale"] => serialise_selected(&self.scale),
            ["refresh_mhz"] => serialise_selected(&self.refresh_mhz),
            ["usable", tail @ ..] => self.usable.select(tail),
            ["presentation", tail @ ..] => select_serialised(self.presentation.as_ref()?, tail),
            _ => None,
        }
    }

    fn node_kind(&self, path: &[&str]) -> Option<SnapshotNodeKind> {
        match path {
            [] | ["usable"] => Some(SnapshotNodeKind::Object),
            ["name" | "default" | "x" | "y" | "width" | "height" | "scale" | "refresh_mhz"] => {
                Some(SnapshotNodeKind::Leaf)
            }
            ["usable", tail @ ..] => self.usable.node_kind(tail),
            ["presentation", tail @ ..] => serialised_node_kind(self.presentation.as_ref()?, tail),
            _ => None,
        }
    }
}

impl InputSnapshot {
    fn select(&self, path: &[&str]) -> Option<Value> {
        match path {
            [] => serialise_selected(self),
            ["seats", tail @ ..] => self
                .seats
                .as_ref()
                .and_then(|seats| select_serialised(seats, tail)),
            ["last_origin"] => serialise_selected(&self.last_origin),
            ["corners", tail @ ..] => self.corners.select(tail),
            ["host"] => self.host.as_ref().and_then(serialise_selected),
            ["host", "passthrough"] => self
                .host
                .as_ref()
                .and_then(|host| serialise_selected(&host.passthrough)),
            _ => None,
        }
    }

    fn node_kind(&self, path: &[&str]) -> Option<SnapshotNodeKind> {
        match path {
            [] | ["corners"] => Some(SnapshotNodeKind::Object),
            ["seats", tail @ ..] => self
                .seats
                .as_ref()
                .and_then(|seats| serialised_node_kind(seats, tail)),
            ["last_origin"] => Some(SnapshotNodeKind::Leaf),
            ["corners", tail @ ..] => self.corners.node_kind(tail),
            ["host"] => self.host.map(|_| SnapshotNodeKind::Object),
            ["host", "passthrough"] => self.host.map(|_| SnapshotNodeKind::Leaf),
            _ => None,
        }
    }
}

impl SurfaceSnapshot {
    fn select(&self, path: &[&str]) -> Option<Value> {
        match path {
            [] => serialise_selected(self),
            ["id"] => serialise_selected(&self.id),
            ["role"] => serialise_selected(&self.role),
            ["mapped"] => serialise_selected(&self.mapped),
            ["visible"] => serialise_selected(&self.visible),
            ["x"] => serialise_selected(&self.x),
            ["y"] => serialise_selected(&self.y),
            ["width"] => serialise_selected(&self.width),
            ["height"] => serialise_selected(&self.height),
            ["band"] => serialise_selected(&self.band),
            ["sequence"] => serialise_selected(&self.sequence),
            ["tree_index"] => serialise_selected(&self.tree_index),
            ["parent"] => serialise_selected(&self.parent),
            ["output"] => serialise_selected(&self.output),
            ["title"] => serialise_selected(&self.title),
            ["app_id"] => serialise_selected(&self.app_id),
            ["focused"] => serialise_selected(&self.focused),
            ["activated"] => serialise_selected(&self.activated),
            ["maximized"] => serialise_selected(&self.maximized),
            ["fullscreen"] => serialise_selected(&self.fullscreen),
            ["minimized"] => serialise_selected(&self.minimized),
            ["workspace"] => serialise_selected(&self.workspace),
            ["decoration"] => serialise_selected(&self.decoration),
            ["layer"] => serialise_selected(&self.layer),
            ["layer", tail @ ..] => self.layer.as_ref()?.select(tail),
            ["foreign_id"] => serialise_selected(&self.foreign_id),
            ["generation"] => serialise_selected(&self.generation),
            _ => select_serialised(&self.occlusion, path),
        }
    }

    fn node_kind(&self, path: &[&str]) -> Option<SnapshotNodeKind> {
        match path {
            [] => Some(SnapshotNodeKind::Object),
            [
                "id" | "role" | "mapped" | "visible" | "x" | "y" | "width" | "height" | "band"
                | "sequence" | "tree_index" | "parent" | "output" | "title" | "app_id" | "focused"
                | "activated" | "maximized" | "fullscreen" | "minimized" | "workspace"
                | "decoration" | "foreign_id" | "generation",
            ] => Some(SnapshotNodeKind::Leaf),
            ["layer"] => Some(if self.layer.is_some() {
                SnapshotNodeKind::Object
            } else {
                SnapshotNodeKind::Leaf
            }),
            ["layer", tail @ ..] => self.layer.as_ref()?.node_kind(tail),
            _ => serialised_node_kind(&self.occlusion, path),
        }
    }
}

/// The `windows.*` row of a mapped xdg toplevel's `surfaces.*` row.
pub fn project_window_row(surface: &SurfaceSnapshot) -> WindowSnapshot {
    WindowSnapshot {
        tiled: surface.window.tiled,
        requested_tiled: surface.window.requested_tiled,
        native_requested_tiled: surface.window.native_requested_tiled,
        tile_pending_reason: surface.window.tile_pending_reason,
        configure_pending: surface.window.configure_pending,
        occlusion: surface.occlusion.clone(),
        id: surface.id,
        foreign_id: surface.foreign_id.clone(),
        title: surface.title.clone(),
        app_id: surface.app_id.clone(),
        x: surface.x,
        y: surface.y,
        width: surface.width,
        height: surface.height,
        focused: surface.focused,
        maximized: surface.maximized,
        fullscreen: surface.fullscreen,
        minimized: surface.minimized,
        output: surface.output.clone(),
        band: surface.band,
        generation: surface.generation,
        window_x: surface.window.window_x,
        window_y: surface.window.window_y,
        window_width: surface.window.window_width,
        window_height: surface.window.window_height,
        visible: surface.visible,
        pid: surface.window.pid,
        workspace: surface.window.workspace,
        presentation: None,
    }
}

/// Which read paths a batch of reads can reach. Volatile presentation
/// leaves are computed only for those (and never for diff baselines).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadScopes {
    All,
    Paths(Vec<String>),
}

impl ReadScopes {
    /// Merge one request's scope (`None` = the whole tree).
    pub fn add(&mut self, scope: Option<&str>) {
        match (&mut *self, scope) {
            (Self::All, _) => {}
            (Self::Paths(_), None) => *self = Self::All,
            (Self::Paths(paths), Some(path)) => paths.push(path.to_string()),
        }
    }

    /// Whether a read under one of the scopes can include `path`: the scope
    /// is `path`, an ancestor of it, or a descendant of it.
    pub fn wants(&self, path: &str) -> bool {
        let related = |scope: &str| {
            scope == path
                || path
                    .strip_prefix(scope)
                    .is_some_and(|rest| rest.starts_with('.'))
                || scope
                    .strip_prefix(path)
                    .is_some_and(|rest| rest.starts_with('.'))
        };
        match self {
            Self::All => true,
            Self::Paths(paths) => paths.iter().any(|scope| related(scope)),
        }
    }
}

/// `wp_presentation_feedback` kind bits by name.
const PRESENTATION_FLAG_NAMES: [(u32, &str); 4] = [
    (0x1, "vsync"),
    (0x2, "hw_clock"),
    (0x4, "hw_completion"),
    (0x8, "zero_copy"),
];

pub fn presentation_flag_names(mask: u32) -> Vec<&'static str> {
    PRESENTATION_FLAG_NAMES
        .iter()
        .filter(|(bit, _)| mask & bit != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// `outputs.<key>.presentation.*` from the output's stats row (none yet:
/// zeroes counted from `epoch_us`).
pub fn output_presentation(
    stats: Option<&OutputStats>,
    epoch_us: u64,
) -> OutputPresentationSnapshot {
    let intervals = stats
        .map(|stats| stats.intervals_us.summary())
        .unwrap_or_default();
    let flags_mask = stats
        .filter(|stats| stats.frames > 0)
        .map(|stats| stats.flags);
    OutputPresentationSnapshot {
        clock_id: CLOCK_MONOTONIC_ID,
        flags: flags_mask.map(presentation_flag_names),
        flags_mask,
        refresh_us: stats.and_then(|stats| stats.refresh_us),
        frames: stats.map_or(0, |stats| stats.frames),
        interval_p50_us: intervals.p50,
        interval_p99_us: intervals.p99,
        since_us: stats.map_or(epoch_us, |stats| stats.since_us),
    }
}

/// Whether output `dropped_output`'s slug `key` is already taken; the first
/// output keeps the key and every later one is dropped and counted
/// (`port.slug_collisions`).
pub fn output_slug_collides(
    outputs: &BTreeMap<String, OutputSnapshot>,
    key: &str,
    dropped_output: &str,
    collisions: &mut u64,
) -> bool {
    let _ = dropped_output;
    if !outputs.contains_key(key) {
        return false;
    }
    *collisions = collisions.saturating_add(1);
    true
}

/// The `o_<slug>` key an output is published under (also the key of its
/// current workspace).
pub fn output_key(name: &str) -> String {
    let mut key = String::from("o_");
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            key.push(character.to_ascii_lowercase());
        } else {
            key.push('_');
        }
    }
    key
}

/// The `surfaces.*` / `windows.*` key of a surface id.
pub fn surface_key(id: u64) -> String {
    format!("s{id}")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DescribeType {
    Bool,
    Number,
    String,
    List,
    Object,
}

impl DescribeType {
    const fn name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Number => "number",
            Self::String => "string",
            Self::List => "list",
            Self::Object => "object",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PatternSegment {
    Literal(&'static str),
    OutputKey,
    SurfaceKey,
    /// A content source id (`[a-z0-9_-]{1,64}`).
    SourceKey,
    SeatKey,
}

#[derive(Clone, Copy, Debug)]
pub struct DescribeEntry {
    pub pattern: &'static [PatternSegment],
    pub ty: DescribeType,
    pub description: &'static str,
    pub mutable: bool,
    pub sensitive: bool,
    pub format: Option<&'static str>,
    pub enum_values: &'static [&'static str],
    pub range: Option<&'static str>,
    pub persistence: Option<&'static str>,
    pub owner: &'static str,
    /// Served by reads but never reported by `props.changed`.
    pub volatile: bool,
}

macro_rules! descriptor {
    ($segments:expr, $ty:ident, $description:expr) => {
        DescribeEntry {
            pattern: $segments,
            ty: DescribeType::$ty,
            description: $description,
            mutable: false,
            sensitive: false,
            format: None,
            enum_values: &[],
            range: None,
            persistence: None,
            owner: "comp",
            volatile: false,
        }
    };
    ($segments:expr, $ty:ident, $description:expr, mutable, range = $range:expr) => {
        DescribeEntry {
            mutable: true,
            range: Some($range),
            persistence: Some("none"),
            ..descriptor!($segments, $ty, $description)
        }
    };
    ($segments:expr, $ty:ident, $description:expr, mutable) => {
        DescribeEntry {
            mutable: true,
            persistence: Some("none"),
            ..descriptor!($segments, $ty, $description)
        }
    };
    ($segments:expr, $ty:ident, $description:expr, format = $format:expr) => {
        DescribeEntry {
            format: Some($format),
            ..descriptor!($segments, $ty, $description)
        }
    };
    ($segments:expr, $ty:ident, $description:expr, enum = $values:expr) => {
        DescribeEntry {
            enum_values: $values,
            ..descriptor!($segments, $ty, $description)
        }
    };
}

macro_rules! volatile {
    ([$($segment:expr),+ $(,)?], $ty:ident, $description:expr) => {
        DescribeEntry {
            volatile: true,
            ..descriptor!(&[$($segment),+], $ty, $description)
        }
    };
}

use PatternSegment::{Literal as L, OutputKey as O, SeatKey as K, SourceKey as C, SurfaceKey as S};

pub static DESCRIPTORS: &[DescribeEntry] = &[
    volatile!(
        [L("input"), L("seats"), K, L("name")],
        String,
        "Advertised wl_seat name"
    ),
    volatile!(
        [L("input"), L("seats"), K, L("keyboard_focus")],
        Object,
        "Keyboard focus identity and generation, or null"
    ),
    volatile!(
        [L("input"), L("seats"), K, L("keyboard_focus"), L("id")],
        Number,
        "Keyboard focus surface id"
    ),
    volatile!(
        [
            L("input"),
            L("seats"),
            K,
            L("keyboard_focus"),
            L("generation")
        ],
        Number,
        "Keyboard focus surface generation"
    ),
    volatile!(
        [L("input"), L("seats"), K, L("pointer_focus")],
        Object,
        "Pointer focus identity and generation, or null"
    ),
    volatile!(
        [L("input"), L("seats"), K, L("pointer_focus"), L("id")],
        Number,
        "Pointer focus surface id"
    ),
    volatile!(
        [
            L("input"),
            L("seats"),
            K,
            L("pointer_focus"),
            L("generation")
        ],
        Number,
        "Pointer focus surface generation"
    ),
    volatile!(
        [L("input"), L("seats"), K, L("pointer")],
        Object,
        "Output-local pointer position, or null when unknown or locked"
    ),
    volatile!(
        [L("input"), L("seats"), K, L("pointer"), L("output")],
        String,
        "Raw protocol output name"
    ),
    volatile!(
        [L("input"), L("seats"), K, L("pointer"), L("x")],
        Number,
        "Output-local logical pointer x"
    ),
    volatile!(
        [L("input"), L("seats"), K, L("pointer"), L("y")],
        Number,
        "Output-local logical pointer y"
    ),
    volatile!(
        [L("input"), L("seats"), K, L("last_input_us")],
        Number,
        "Last input CLOCK_MONOTONIC microseconds, or null before input"
    ),
    volatile!(
        [L("input"), L("last_origin")],
        String,
        "human, agent, or null before input"
    ),
    descriptor!(
        &[L("surfaces"), S, L("occluded")],
        Bool,
        "Entire canonical family is covered on every intersecting output"
    ),
    descriptor!(
        &[L("surfaces"), S, L("occlusion_reason")],
        String,
        "unknown, exposed, or opaque-coverage"
    ),
    descriptor!(
        &[L("surfaces"), S, L("occlusion_revision")],
        Number,
        "Revision of the latest visibility decision transition"
    ),
    volatile!(
        [L("occlusion"), L("counters"), L("withheld_opportunities")],
        Number,
        "Compositor-wide occlusion counter; read-only, never diffed"
    ),
    volatile!(
        [L("occlusion"), L("counters"), L("resumes")],
        Number,
        "Surface trees resumed by delivering retained callbacks; read-only, never diffed"
    ),
    volatile!(
        [L("occlusion"), L("counters"), L("recomputes")],
        Number,
        "Compositor-wide occlusion counter; read-only, never diffed"
    ),
    volatile!(
        [L("occlusion"), L("counters"), L("conservative_fallbacks")],
        Number,
        "Compositor-wide occlusion counter; read-only, never diffed"
    ),
    descriptor!(
        &[L("windows"), S, L("occluded")],
        Bool,
        "Entire canonical family is covered on every intersecting output"
    ),
    descriptor!(
        &[L("windows"), S, L("occlusion_reason")],
        String,
        "unknown, exposed, or opaque-coverage"
    ),
    descriptor!(
        &[L("windows"), S, L("occlusion_revision")],
        Number,
        "Revision of the latest visibility decision transition"
    ),
    descriptor!(
        &[L("info"), L("service")],
        String,
        "Registered Bus service name"
    ),
    descriptor!(
        &[L("info"), L("version")],
        String,
        "Compositor build version"
    ),
    descriptor!(&[L("info"), L("backend")], String, "Active compositor backend", enum = &["nested", "kms"]),
    descriptor!(&[L("info"), L("engine")], String, "Rendering engine"),
    descriptor!(
        &[L("info"), L("instance")],
        String,
        "Random per-process compositor instance id"
    ),
    descriptor!(
        &[L("info"), L("explicit_sync_advertised")],
        Bool,
        "Explicit-sync protocol global currently advertised to clients"
    ),
    descriptor!(
        &[L("info"), L("explicit_sync_healthy")],
        Bool,
        "Explicit-sync retirement pipeline has not permanently faulted"
    ),
    descriptor!(
        &[L("outputs"), O, L("name")],
        String,
        "Raw protocol output name"
    ),
    descriptor!(
        &[L("outputs"), O, L("default")],
        Bool,
        "Whether this is the default output"
    ),
    descriptor!(
        &[L("outputs"), O, L("x")],
        Number,
        "Logical output x origin",
        format = "logical_px"
    ),
    descriptor!(
        &[L("outputs"), O, L("y")],
        Number,
        "Logical output y origin",
        format = "logical_px"
    ),
    descriptor!(
        &[L("outputs"), O, L("width")],
        Number,
        "Logical output width",
        format = "logical_px"
    ),
    descriptor!(
        &[L("outputs"), O, L("height")],
        Number,
        "Logical output height",
        format = "logical_px"
    ),
    descriptor!(
        &[L("outputs"), O, L("scale")],
        Number,
        "Fractional output scale",
        format = "scale_factor"
    ),
    descriptor!(
        &[L("outputs"), O, L("refresh_mhz")],
        Number,
        "Output refresh rate",
        format = "millihertz"
    ),
    descriptor!(
        &[L("outputs"), O, L("usable"), L("x")],
        Number,
        "Usable logical x origin",
        format = "logical_px"
    ),
    descriptor!(
        &[L("outputs"), O, L("usable"), L("y")],
        Number,
        "Usable logical y origin",
        format = "logical_px"
    ),
    descriptor!(
        &[L("outputs"), O, L("usable"), L("width")],
        Number,
        "Usable logical width",
        format = "logical_px"
    ),
    descriptor!(
        &[L("outputs"), O, L("usable"), L("height")],
        Number,
        "Usable logical height",
        format = "logical_px"
    ),
    descriptor!(
        &[L("surfaces"), S, L("id")],
        Number,
        "Session-local surface id",
        format = "surface_id"
    ),
    descriptor!(&[L("surfaces"), S, L("role")], String, "Wayland surface role", enum = &["toplevel", "popup", "layer", "subsurface", "lock"]),
    descriptor!(
        &[L("surfaces"), S, L("mapped")],
        Bool,
        "Whether the surface has mapped protocol content"
    ),
    descriptor!(
        &[L("surfaces"), S, L("visible")],
        Bool,
        "Effective scene visibility including ancestors"
    ),
    descriptor!(
        &[L("surfaces"), S, L("x")],
        Number,
        "Surface x origin",
        format = "logical_px"
    ),
    descriptor!(
        &[L("surfaces"), S, L("y")],
        Number,
        "Surface y origin",
        format = "logical_px"
    ),
    descriptor!(
        &[L("surfaces"), S, L("width")],
        Number,
        "Surface width",
        format = "logical_px"
    ),
    descriptor!(
        &[L("surfaces"), S, L("height")],
        Number,
        "Surface height",
        format = "logical_px"
    ),
    descriptor!(&[L("surfaces"), S, L("band")], String, "Compositor stack band", enum = &["background", "bottom", "normal", "top", "overlay", "lock"]),
    descriptor!(
        &[L("surfaces"), S, L("sequence")],
        Number,
        "Root ordering sequence"
    ),
    descriptor!(
        &[L("surfaces"), S, L("tree_index")],
        Number,
        "Within-tree ordering index"
    ),
    descriptor!(
        &[L("surfaces"), S, L("parent")],
        Number,
        "Parent surface id or null",
        format = "surface_id"
    ),
    descriptor!(
        &[L("surfaces"), S, L("output")],
        String,
        "Output key or null"
    ),
    descriptor!(
        &[L("surfaces"), S, L("title")],
        String,
        "Cached toplevel title or null"
    ),
    descriptor!(
        &[L("surfaces"), S, L("app_id")],
        String,
        "Cached toplevel app id or null"
    ),
    descriptor!(
        &[L("surfaces"), S, L("focused")],
        Bool,
        "Current focus-arbiter decision"
    ),
    descriptor!(
        &[L("surfaces"), S, L("activated")],
        Bool,
        "XDG activation decision from the same focus edge"
    ),
    descriptor!(
        &[L("surfaces"), S, L("maximized")],
        Bool,
        "Committed maximized state"
    ),
    descriptor!(
        &[L("surfaces"), S, L("fullscreen")],
        Bool,
        "Committed Wayland fullscreen state"
    ),
    descriptor!(
        &[L("surfaces"), S, L("minimized")],
        Bool,
        "Compositor minimized state"
    ),
    descriptor!(
        &[L("surfaces"), S, L("workspace")],
        Number,
        "1-based workspace of a mapped managed toplevel (X11 included), else null; write windows.s<id>.workspace to move"
    ),
    descriptor!(&[L("surfaces"), S, L("decoration")], String, "Committed decoration mode or null", enum = &["server", "client", "unbound"]),
    descriptor!(
        &[L("surfaces"), S, L("layer")],
        Object,
        "Layer metadata object or null"
    ),
    descriptor!(&[L("surfaces"), S, L("layer"), L("stratum")], String, "Committed layer-shell stratum", enum = &["background", "bottom", "top", "overlay"]),
    descriptor!(&[L("surfaces"), S, L("layer"), L("interactivity")], String, "Committed layer keyboard interactivity", enum = &["none", "on_demand", "exclusive"]),
    descriptor!(
        &[L("surfaces"), S, L("layer"), L("exclusive_zone")],
        Number,
        "Applied layer exclusive zone",
        format = "logical_px"
    ),
    descriptor!(&[L("surfaces"), S, L("layer"), L("binding")], String, "Layer output binding", enum = &["explicit", "default"]),
    descriptor!(
        &[L("surfaces"), S, L("foreign_id")],
        String,
        "Mapped foreign-toplevel identifier or null"
    ),
    descriptor!(
        &[L("surfaces"), S, L("generation")],
        Number,
        "Role generation; a new role (including the role ending) takes a new value, an unmap/remap of the same role keeps it"
    ),
    descriptor!(
        &[L("windows"), S, L("id")],
        Number,
        "Session-local toplevel id",
        format = "surface_id"
    ),
    descriptor!(
        &[L("windows"), S, L("foreign_id")],
        String,
        "Mapped foreign-toplevel identifier"
    ),
    descriptor!(
        &[L("windows"), S, L("title")],
        String,
        "Cached toplevel title"
    ),
    descriptor!(
        &[L("windows"), S, L("app_id")],
        String,
        "Cached toplevel app id"
    ),
    descriptor!(
        &[L("windows"), S, L("x")],
        Number,
        "Toplevel x origin",
        format = "logical_px"
    ),
    descriptor!(
        &[L("windows"), S, L("y")],
        Number,
        "Toplevel y origin",
        format = "logical_px"
    ),
    descriptor!(
        &[L("windows"), S, L("width")],
        Number,
        "Toplevel buffer width, CSD shadow included; window_width is the window-geometry extent",
        format = "logical_px"
    ),
    descriptor!(
        &[L("windows"), S, L("height")],
        Number,
        "Toplevel buffer height, CSD shadow included; window_height is the window-geometry extent",
        format = "logical_px"
    ),
    descriptor!(
        &[L("windows"), S, L("focused")],
        Bool,
        "Whether this toplevel owns keyboard focus"
    ),
    descriptor!(
        &[L("windows"), S, L("maximized")],
        Bool,
        "Committed maximized state; writes request a configure",
        mutable
    ),
    descriptor!(
        &[L("windows"), S, L("fullscreen")],
        Bool,
        "Committed fullscreen state; writes request a configure",
        mutable
    ),
    descriptor!(
        &[L("windows"), S, L("minimized")],
        Bool,
        "Compositor minimized state; write false to restore and focus this window, true to minimise it",
        mutable
    ),
    descriptor!(
        &[L("windows"), S, L("tiled")],
        Bool,
        "Client-committed native tiled flags; membership is separate"
    ),
    descriptor!(
        &[L("windows"), S, L("requested_tiled")],
        Bool,
        "Persistent generation-fenced tile membership, including overlays and pending groups"
    ),
    descriptor!(
        &[L("windows"), S, L("native_requested_tiled")],
        Bool,
        "Native requested tiled flags; false during overlays or pending normal return"
    ),
    descriptor!(&[L("windows"), S, L("tile_pending_reason")], String, "Complete-group layout failure or null", enum = &["no_output", "invalid_area", "capacity", "invalid_constraints", "insufficient_area"]),
    descriptor!(
        &[L("windows"), S, L("configure_pending")],
        Bool,
        "Requested native state or tiled slot differs from committed client state/geometry"
    ),
    descriptor!(
        &[L("windows"), S, L("output")],
        String,
        "Output key or null"
    ),
    // Writable (bottom|normal) and process-lifetime; the enum still lists
    // every band a read can report.
    DescribeEntry {
        mutable: true,
        persistence: Some("none"),
        ..descriptor!(&[L("windows"), S, L("band")], String, "Compositor stack band; writable as bottom|normal to demote a window behind all normal windows or restore it", enum = &["background", "bottom", "normal", "top", "overlay", "lock"])
    },
    descriptor!(
        &[L("windows"), S, L("generation")],
        Number,
        "Role generation (same value as surfaces.s<id>.generation); {id, generation} names one window"
    ),
    descriptor!(
        &[L("windows"), S, L("window_x")],
        Number,
        "Window-geometry x origin (x/y are the buffer origin, CSD shadow included); the buffer stands on a whole physical pixel, so this can be fractional at a fractional scale (1.2 at 2.5x) and is an integer at scale 1",
        format = "logical_px"
    ),
    descriptor!(
        &[L("windows"), S, L("window_y")],
        Number,
        "Window-geometry y origin; fractional at a fractional scale like window_x, an integer at scale 1",
        format = "logical_px"
    ),
    descriptor!(
        &[L("windows"), S, L("window_width")],
        Number,
        "Window-geometry width, excluding CSD shadow when the client sets geometry (width/height are the buffer extent, shadow included); without explicit geometry, uses committed surface-tree bounds like window_x/window_y, or the root buffer if no geometry is cached",
        format = "logical_px"
    ),
    descriptor!(
        &[L("windows"), S, L("window_height")],
        Number,
        "Window-geometry height, excluding CSD shadow when the client sets geometry (width/height are the buffer extent, shadow included); without explicit geometry, uses committed surface-tree bounds like window_x/window_y, or the root buffer if no geometry is cached",
        format = "logical_px"
    ),
    descriptor!(
        &[L("windows"), S, L("visible")],
        Bool,
        "Whether the window is effectively on screen (false while minimised)"
    ),
    descriptor!(
        &[L("windows"), S, L("pid")],
        Number,
        "Process id of the client socket peer (a proxy or sandbox may report its own), or null"
    ),
    descriptor!(
        &[L("windows"), S, L("workspace")],
        Number,
        "1-based workspace of this window; a write moves it there without switching (off the current workspace it reads visible:false, minimized:false)",
        mutable,
        range = "1..=count"
    ),
    descriptor!(
        &[L("workspaces"), L("count")],
        Number,
        "Number of workspaces; shrinking moves stranded windows to the last one and clamps every current",
        mutable,
        range = "1..=16"
    ),
    descriptor!(
        &[L("workspaces"), L("current")],
        Number,
        "The default output's current workspace (1-based); a write switches",
        mutable,
        range = "1..=count"
    ),
    descriptor!(
        &[L("workspaces"), O, L("current")],
        Number,
        "This output's current workspace (1-based); a write switches it (only the default output is switchable in 0.59)",
        mutable,
        range = "1..=count"
    ),
    descriptor!(
        &[L("workspaces"), L("list")],
        List,
        "One {index, windows} row per workspace; windows counts the mapped managed toplevels on it (X11 included: the surfaces.* rows with a workspace)"
    ),
    descriptor!(
        &[L("stack")],
        List,
        "Mapped root surface ids from top to bottom",
        format = "surface_id"
    ),
    descriptor!(
        &[L("focus"), L("keyboard")],
        Number,
        "Keyboard-focused surface id or null",
        format = "surface_id"
    ),
    descriptor!(
        &[L("focus"), L("exclusive_latch")],
        Number,
        "Exclusive layer focus latch or null",
        format = "surface_id"
    ),
    descriptor!(
        &[L("focus"), L("pointer")],
        Number,
        "Pointer-focused surface id or null",
        format = "surface_id"
    ),
    descriptor!(&[L("focus"), L("pointer_grab")], String, "Active pointer grab kind", enum = &["none", "chrome", "move", "resize", "popup"]),
    descriptor!(
        &[L("focus"), L("window"), L("id")],
        Number,
        "Keyboard-focused managed window (xdg or X11) id or null",
        format = "surface_id"
    ),
    descriptor!(
        &[L("focus"), L("window"), L("generation")],
        Number,
        "Role generation of the keyboard-focused window or null"
    ),
    descriptor!(&[L("focus"), L("session_lock")], String, "Session-lock observation state", enum = &["none", "locking", "locked", "orphaned", "unlocking"]),
    descriptor!(
        &[L("decoration"), L("enabled")],
        Bool,
        "Whether server-side decoration is enabled"
    ),
    descriptor!(&[L("decoration"), L("style")], String, "Startup decoration style", enum = &["mac", "win11", "mixos"]),
    descriptor!(
        &[L("bindings"), L("enabled")],
        Bool,
        "Whether normal compositor key interception is enabled"
    ),
    descriptor!(&[L("bindings"), L("profile")], String, "Compiled binding profile", enum = &["nested", "kms-live"]),
    descriptor!(
        &[L("bindings"), L("table")],
        List,
        "Compiled keybinding chord/action rows"
    ),
    descriptor!(
        &[L("input"), L("corners"), L("holders")],
        Bool,
        "Whether the panel holder control plane is available"
    ),
    descriptor!(
        &[L("input"), L("corners"), L("enabled")],
        Bool,
        "Whether compositor hot-corner detection is enabled",
        mutable
    ),
    descriptor!(
        &[L("input"), L("corners"), L("deadzone_px")],
        Number,
        "Corner deadzone in logical pixels",
        mutable,
        range = "1.0..=256.0"
    ),
    descriptor!(
        &[L("input"), L("corners"), L("dwell_ms")],
        Number,
        "Velocity-qualified corner dwell in milliseconds",
        mutable,
        range = "0..=5000"
    ),
    descriptor!(
        &[L("input"), L("corners"), L("velocity_max_px_s")],
        Number,
        "Maximum corner-entry velocity in logical pixels per second",
        mutable,
        range = "1.0..=20000.0"
    ),
    descriptor!(
        &[L("input"), L("corners"), L("affordance")],
        Bool,
        "Whether comp draws the hotspot hover reveal, release flash and discovery flash",
        mutable
    ),
    descriptor!(
        &[L("input"), L("corners"), L("discovery")],
        Bool,
        "Whether every hotspot flashes slowly until the first corner reveal",
        mutable
    ),
    volatile!(
        [L("input"), L("corners"), L("enforced"), L("top")],
        Number,
        "Top-edge shell layers comp hides and excludes from input for an unapplied conceal; read-only, never diffed"
    ),
    volatile!(
        [L("input"), L("corners"), L("enforced"), L("bottom")],
        Number,
        "Bottom-edge shell layers comp hides and excludes from input for an unapplied conceal; read-only, never diffed"
    ),
    volatile!(
        [L("input"), L("corners"), L("enforced"), L("left")],
        Number,
        "Left-edge shell layers comp hides and excludes from input for an unapplied conceal; read-only, never diffed"
    ),
    volatile!(
        [L("input"), L("corners"), L("enforced"), L("right")],
        Number,
        "Right-edge shell layers comp hides and excludes from input for an unapplied conceal; read-only, never diffed"
    ),
    volatile!(
        [L("input"), L("corners"), L("held"), L("top")],
        Number,
        "Explicit comp.panel.hold holds recorded for top-edge panels; read-only, never diffed"
    ),
    volatile!(
        [L("input"), L("corners"), L("held"), L("bottom")],
        Number,
        "Explicit comp.panel.hold holds recorded for bottom-edge panels; read-only, never diffed"
    ),
    volatile!(
        [L("input"), L("corners"), L("held"), L("left")],
        Number,
        "Explicit comp.panel.hold holds recorded for left-edge panels; read-only, never diffed"
    ),
    volatile!(
        [L("input"), L("corners"), L("held"), L("right")],
        Number,
        "Explicit comp.panel.hold holds recorded for right-edge panels; read-only, never diffed"
    ),
    descriptor!(
        &[L("input"), L("host"), L("passthrough")],
        Bool,
        "Nested backend only: false drops host pointer and key input (resize, \
         scale and pointer leave still pass) so injected input is not overwritten",
        mutable
    ),
    // The one file-persisted leaf on this surface (see the resolver in
    // xwayland.rs for why startup-read + persistence:none would make the
    // leaf decorative). `persistence: "file"` overrides the mutable
    // macro-arm's "none".
    #[cfg(feature = "xwayland")]
    DescribeEntry {
        persistence: Some("file"),
        ..descriptor!(
            &[L("xwayland"), L("enabled")],
            Bool,
            "Whether this compositor spawns XWayland; read at startup, a write persists \
             for the NEXT startup (no live toggle; the Set reply's `persisted` field \
             reports write durability). COMPD_XWAYLAND overrides at launch",
            mutable
        )
    },
    #[cfg(feature = "xwayland")]
    descriptor!(
        &[L("xwayland"), L("persist_path")],
        String,
        "Resolved per-socket file xwayland.enabled persists to (root- and \
         socket-dependent; read-only so the governing file is visible, not deduced)"
    ),
    #[cfg(feature = "xwayland")]
    descriptor!(
        &[L("xwayland"), L("display")],
        String,
        "The X display this compositor's Xwayland serves (\":N\"); null until the \
         generation is ready and again after it goes down"
    ),
    #[cfg(feature = "xwayland")]
    descriptor!(
        &[L("xwayland"), L("state")],
        String,
        "The X server's lifecycle: off, starting, ready, retrying (it died; the one \
         restart is armed) or failed (died again; down until the compositor restarts)"
    ),
    #[cfg(feature = "xwayland")]
    descriptor!(
        &[L("xwayland"), L("failures")],
        Number,
        "X server deaths since this compositor started, startup crashes included"
    ),
    // Observed linux-dmabuf imports: what the driver actually
    // accepted, not what it advertised. In memory only — a compositor restart
    // starts from zero — and never diffed (a refusal storm would flood
    // props.changed).
    volatile!(
        [L("dmabuf"), L("accepted")],
        Number,
        "linux-dmabuf imports comp accepted since this compositor started (reset on restart)"
    ),
    volatile!(
        [L("dmabuf"), L("failed")],
        Number,
        "linux-dmabuf imports comp refused since this compositor started (reset on restart)"
    ),
    volatile!(
        [L("dmabuf"), L("failures")],
        List,
        "The newest 16 refused imports, oldest first: {format (fourcc), modifier (hex), \
         reason (invalid_metadata|descriptor_dup_failed|queue_full|worker_stopped|\
         vulkan_rejected|probe_panicked|probe_retired), detail, at_us (CLOCK_MONOTONIC)}; \
         not persisted"
    ),
    // Presentation statistics are volatile: served by get/list/describe,
    // never diffed into props.changed (a watched 60 Hz client would flood
    // the topic). Times are CLOCK_MONOTONIC µs.
    volatile!(
        [L("windows"), S, L("presentation"), L("presented")],
        Number,
        "Content updates shown since since_us (one per frame, subsurfaces included)"
    ),
    volatile!(
        [L("windows"), S, L("presentation"), L("discarded")],
        Number,
        "Content updates superseded before any frame showed them"
    ),
    volatile!(
        [L("windows"), S, L("presentation"), L("last_presented_us")],
        Number,
        "Time of the newest frame that showed an update, or null"
    ),
    volatile!(
        [L("windows"), S, L("presentation"), L("interval_p50_us")],
        Number,
        "Median interval between consecutive presentations while shown (newest 512), or null"
    ),
    volatile!(
        [L("windows"), S, L("presentation"), L("interval_p99_us")],
        Number,
        "99th percentile interval between consecutive presentations (newest 512), or null"
    ),
    volatile!(
        [L("windows"), S, L("presentation"), L("interval_max_us")],
        Number,
        "Largest interval between consecutive presentations (newest 512), or null"
    ),
    volatile!(
        [
            L("windows"),
            S,
            L("presentation"),
            L("commit_to_present_p50_us")
        ],
        Number,
        "Median buffer commit to presentation latency (newest 512), or null"
    ),
    volatile!(
        [
            L("windows"),
            S,
            L("presentation"),
            L("commit_to_present_p99_us")
        ],
        Number,
        "99th percentile buffer commit to presentation latency (newest 512), or null"
    ),
    volatile!(
        [
            L("windows"),
            S,
            L("presentation"),
            L("input_to_present_p50_us")
        ],
        Number,
        "Median injected input to first presented update committed after it, or null"
    ),
    volatile!(
        [
            L("windows"),
            S,
            L("presentation"),
            L("input_to_present_p99_us")
        ],
        Number,
        "99th percentile injected input to presentation latency, or null"
    ),
    volatile!(
        [L("windows"), S, L("presentation"), L("missed")],
        Number,
        "Vblanks skipped while an update was pending; null while the refresh is unknown (nested)"
    ),
    volatile!(
        [L("windows"), S, L("presentation"), L("refresh_us")],
        Number,
        "Fixed refresh of the newest presentation's output, or null (unknown or variable)"
    ),
    volatile!(
        [L("windows"), S, L("presentation"), L("since_us")],
        Number,
        "When counting started (the window's first update or the last reset)"
    ),
    volatile!(
        [L("outputs"), O, L("presentation"), L("clock_id")],
        Number,
        "Presentation clock id (1 = CLOCK_MONOTONIC)"
    ),
    volatile!(
        [L("outputs"), O, L("presentation"), L("flags")],
        List,
        "Kind flags of the newest frame (vsync, hw_clock, hw_completion, zero_copy), or null"
    ),
    volatile!(
        [L("outputs"), O, L("presentation"), L("flags_mask")],
        Number,
        "wp_presentation_feedback kind bits of the newest frame, or null"
    ),
    volatile!(
        [L("outputs"), O, L("presentation"), L("refresh_us")],
        Number,
        "Fixed refresh reported with the newest frame, or null (unknown or variable; never 0)"
    ),
    volatile!(
        [L("outputs"), O, L("presentation"), L("frames")],
        Number,
        "Frames presented on this output since since_us"
    ),
    volatile!(
        [L("outputs"), O, L("presentation"), L("interval_p50_us")],
        Number,
        "Median interval between presented frames (newest 512), or null"
    ),
    volatile!(
        [L("outputs"), O, L("presentation"), L("interval_p99_us")],
        Number,
        "99th percentile interval between presented frames (newest 512), or null"
    ),
    volatile!(
        [L("outputs"), O, L("presentation"), L("since_us")],
        Number,
        "When counting started (compositor start or the last reset)"
    ),
    volatile!(
        [L("sources"), C, L("output")],
        String,
        "Output the content source asked to be measured on, or null for any"
    ),
    volatile!(
        [L("sources"), C, L("registered_at_us")],
        Number,
        "When this registration of the id began"
    ),
    volatile!(
        [L("sources"), C, L("revision")],
        Number,
        "Newest content revision reported by the source"
    ),
    volatile!(
        [L("sources"), C, L("registration")],
        Number,
        "Registration number; a new one each time the id is registered"
    ),
    volatile!(
        [L("sources"), C, L("presentation"), L("presented")],
        Number,
        "Revisions shown since since_us"
    ),
    volatile!(
        [L("sources"), C, L("presentation"), L("discarded")],
        Number,
        "Revisions superseded before any frame showed them"
    ),
    volatile!(
        [L("sources"), C, L("presentation"), L("last_presented_us")],
        Number,
        "Time of the newest frame that showed a revision, or null"
    ),
    volatile!(
        [L("sources"), C, L("presentation"), L("interval_p50_us")],
        Number,
        "Median interval between consecutive presentations while shown (newest 512), or null"
    ),
    volatile!(
        [L("sources"), C, L("presentation"), L("interval_p99_us")],
        Number,
        "99th percentile interval between consecutive presentations (newest 512), or null"
    ),
    volatile!(
        [L("sources"), C, L("presentation"), L("interval_max_us")],
        Number,
        "Largest interval between consecutive presentations (newest 512), or null"
    ),
    volatile!(
        [
            L("sources"),
            C,
            L("presentation"),
            L("commit_to_present_p50_us")
        ],
        Number,
        "Median time from comp first seeing a revision to its presentation, or null"
    ),
    volatile!(
        [
            L("sources"),
            C,
            L("presentation"),
            L("commit_to_present_p99_us")
        ],
        Number,
        "99th percentile time from comp first seeing a revision to its presentation, or null"
    ),
    volatile!(
        [
            L("sources"),
            C,
            L("presentation"),
            L("input_to_present_p50_us")
        ],
        Number,
        "Median injected input to presentation of the revision that answered it, or null"
    ),
    volatile!(
        [
            L("sources"),
            C,
            L("presentation"),
            L("input_to_present_p99_us")
        ],
        Number,
        "99th percentile injected input to presentation latency, or null"
    ),
    volatile!(
        [L("sources"), C, L("presentation"), L("missed")],
        Number,
        "Vblanks skipped while a revision was pending; null while the refresh is unknown (nested)"
    ),
    volatile!(
        [L("sources"), C, L("presentation"), L("refresh_us")],
        Number,
        "Fixed refresh of the newest presentation's output, or null (unknown or variable)"
    ),
    volatile!(
        [L("sources"), C, L("presentation"), L("since_us")],
        Number,
        "When counting started (registration or the last reset)"
    ),
    volatile!(
        [L("sources"), C, L("presentation"), L("upload_bytes_total")],
        Number,
        "GPU upload bytes the source reported since since_us"
    ),
    volatile!(
        [L("sources"), C, L("presentation"), L("damage_px_total")],
        Number,
        "Damaged physical pixels the source reported since since_us"
    ),
    volatile!(
        [L("sources"), C, L("presentation"), L("upload_bytes_p50")],
        Number,
        "Median upload bytes per reported frame (newest 512), or null"
    ),
    volatile!(
        [L("sources"), C, L("presentation"), L("upload_bytes_p99")],
        Number,
        "99th percentile upload bytes per reported frame (newest 512), or null"
    ),
    volatile!(
        [L("sources"), C, L("presentation"), L("damage_px_p50")],
        Number,
        "Median damaged pixels per reported frame (newest 512), or null"
    ),
    volatile!(
        [L("sources"), C, L("presentation"), L("damage_px_p99")],
        Number,
        "99th percentile damaged pixels per reported frame (newest 512), or null"
    ),
    descriptor!(&[L("port"), L("level")], String, "Implemented property substrate level", enum = &["L2"]),
    descriptor!(
        &[L("port"), L("event_seq")],
        Number,
        "Global compositor observation event sequence"
    ),
    descriptor!(
        &[L("port"), L("lost_count")],
        Number,
        "Cumulative compositor observation records lost"
    ),
    descriptor!(
        &[L("port"), L("queue_depth")],
        Number,
        "Accepted port reads and controls not yet completed"
    ),
    descriptor!(
        &[L("port"), L("reply_timeouts")],
        Number,
        "Reply send abandoned after 2 s; delivery not guaranteed (the client sink may still flush it); also counts saturated reply lanes"
    ),
    descriptor!(
        &[L("port"), L("publish_timeouts")],
        Number,
        "Topic publication failures and timeouts"
    ),
    descriptor!(
        &[L("port"), L("slug_collisions")],
        Number,
        "Outputs omitted because their public slug collided with an earlier output"
    ),
    descriptor!(&[L("port"), L("broker")], String, "Live broker connection state", enum = &["connected", "retrying"]),
];

impl DescribeEntry {
    /// Whether this descriptor describes `path`.
    pub fn matches(self, path: &PropPath) -> bool {
        let segments = path.segments().collect::<Vec<_>>();
        segments.len() == self.pattern.len()
            && segments
                .iter()
                .zip(self.pattern)
                .all(|(actual, expected)| match expected {
                    PatternSegment::Literal(expected) => actual == expected,
                    PatternSegment::OutputKey => actual.starts_with("o_") && actual.len() > 2,
                    PatternSegment::SeatKey => matches!(*actual, "human" | "agent"),
                    PatternSegment::SurfaceKey => actual.strip_prefix('s').is_some_and(|id| {
                        !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
                    }),
                    PatternSegment::SourceKey => {
                        (1..=64).contains(&actual.len())
                            && actual.bytes().all(|byte| {
                                byte.is_ascii_lowercase()
                                    || byte.is_ascii_digit()
                                    || byte == b'_'
                                    || byte == b'-'
                            })
                    }
                })
    }
}

#[derive(Serialize)]
struct DescribeReply<'a> {
    path: &'a str,
    #[serde(rename = "type")]
    ty: &'a str,
    mutable: bool,
    sensitive: bool,
    description: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<&'a str>,
    #[serde(rename = "enum", skip_serializing_if = "slice_is_empty")]
    enum_values: &'a [&'a str],
    #[serde(skip_serializing_if = "Option::is_none")]
    range: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    persistence: Option<&'a str>,
    owner: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    children: Option<Vec<String>>,
    #[serde(skip_serializing_if = "is_false")]
    volatile: bool,
}

fn slice_is_empty(values: &&[&str]) -> bool {
    values.is_empty()
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Paths `props.changed` never reports: presentation statistics and the
/// content-source registry change every frame; the holder-plane counts
/// (`input.corners.enforced.*`, `input.corners.held.*`) are read-only
/// diagnostics served only by reads.
pub fn volatile_path(path: &str) -> bool {
    path == "sources"
        || path == "input.last_origin"
        || path == "input.seats"
        || path.starts_with("input.seats.")
        || path.starts_with("sources.")
        || path == "input.corners.enforced"
        || path.starts_with("input.corners.enforced.")
        || path == "input.corners.held"
        || path.starts_with("input.corners.held.")
        || path.split('.').any(|segment| segment == "presentation")
        || path.starts_with("occlusion.counters.")
        || path == "dmabuf"
        || path.starts_with("dmabuf.")
}
/// Answer one read verb from a snapshot. Synchronous and possibly heavy (a
/// full-tree read serialises the whole tree once per snapshot): the
/// transport runs it off the compositor thread.
pub fn dispatch_read(snapshot: &CompSnapshot, command: &str, args: &Value) -> (u8, Arc<str>) {
    dispatch_read_with_limit(snapshot, command, args, MAX_REPLY_BODY_BYTES)
}

pub fn dispatch_read_with_limit(
    snapshot: &CompSnapshot,
    command: &str,
    args: &Value,
    limit_bytes: usize,
) -> (u8, Arc<str>) {
    if command == "comp.info" {
        return enforce_reply_limit(
            (
                0,
                Arc::from(
                    json!({
                        "service": snapshot.info.service,
                        "version": snapshot.info.version,
                        "backend": snapshot.info.backend,
                        "engine": snapshot.info.engine,
                        "instance": snapshot.info.instance,
                        "output_count": snapshot.outputs.len(),
                        "surface_count": snapshot.surfaces.len(),
                        "event_seq": snapshot.port.event_seq,
                        "lost_count": snapshot.port.lost_count,
                    })
                    .to_string(),
                ),
            ),
            limit_bytes,
        );
    }
    if command == "comp.windows.list" {
        return enforce_reply_limit(windows_list(snapshot, args), limit_bytes);
    }
    if !matches!(
        command,
        "comp.props.get" | "comp.props.list" | "comp.props.describe"
    ) {
        return error("unknown_verb");
    }
    if command == "comp.props.get" {
        match optional_path(args, "path") {
            Ok(None) => {
                return full_tree(snapshot).map_or_else(
                    |()| error("busy"),
                    |reply| enforce_measured_reply_limit(reply, limit_bytes),
                );
            }
            Ok(Some(_)) => {}
            Err(()) => return error("unknown_path"),
        }
    }
    let reply = dispatch_selected_read(snapshot, command, args);
    enforce_reply_limit(reply, limit_bytes)
}

fn list_argument(path: &str, expected: &'static str, range: &'static str) -> (u8, Arc<str>) {
    ControlReply::Validation(SetValidationError::InvalidValue {
        path: path.into(),
        expected,
        range,
    })
    .into_wire()
}

/// `comp.windows.list {app_id?, title?, title_contains?, visible?,
/// workspace?}`: the window rows matching every given filter, in id order.
/// `workspace` is an index, `"current"` (this snapshot's current workspace)
/// or `"all"` (the default: no filter, so 0.58 callers see the same set).
fn windows_list(snapshot: &CompSnapshot, args: &Value) -> (u8, Arc<str>) {
    const ALLOWED: &[&str] = &["app_id", "title", "title_contains", "visible", "workspace"];
    let empty = serde_json::Map::new();
    let object = match args {
        Value::Null => &empty,
        Value::Object(object) => object,
        _ => return list_argument("args", "JSON object", "filter object"),
    };
    if let Some(field) = object
        .keys()
        .find(|field| !ALLOWED.contains(&field.as_str()))
    {
        return ControlReply::InvalidArgs {
            field: field.clone(),
            allowed: ALLOWED,
        }
        .into_wire();
    }
    let mut texts = [None; 3];
    for (slot, name) in texts.iter_mut().zip(["app_id", "title", "title_contains"]) {
        match object.get(name) {
            None | Some(Value::Null) => {}
            Some(Value::String(value)) if value.len() <= 4096 => *slot = Some(value.as_str()),
            Some(_) => return list_argument(name, "string", "at most 4096 bytes"),
        }
    }
    let [app_id, title, title_contains] = texts;
    let visible = match object.get("visible") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(visible)) => Some(*visible),
        Some(_) => return list_argument("visible", "bool", "true|false"),
    };
    // Rule 4: `"current"` is resolved against this snapshot's current
    // workspace, so the reply is consistent with the rows it lists. An
    // index above the count is refused like every other workspace input
    // (props.set, the ingress gate): "no such workspace", not "no windows
    // there".
    const WORKSPACE_RANGE: &str = "1..=count|current|all";
    let workspace = match object.get("workspace") {
        None | Some(Value::Null) => None,
        Some(Value::String(word)) if word == "all" => None,
        Some(Value::String(word)) if word == "current" => Some(snapshot.workspaces.current),
        Some(Value::Number(number)) => match number
            .as_u64()
            .and_then(|index| u32::try_from(index).ok())
        {
            Some(index) if (1..=snapshot.workspaces.count).contains(&index) => Some(index),
            _ => {
                return list_argument("workspace", "unsigned integer or string", WORKSPACE_RANGE);
            }
        },
        Some(_) => {
            return list_argument("workspace", "unsigned integer or string", WORKSPACE_RANGE);
        }
    };
    let mut rows = snapshot
        .windows
        .values()
        .filter(|row| {
            app_id.is_none_or(|app_id| row.app_id.as_deref() == Some(app_id))
                && title.is_none_or(|title| row.title.as_deref() == Some(title))
                && title_contains.is_none_or(|needle| {
                    row.title
                        .as_deref()
                        .is_some_and(|title| title.contains(needle))
                })
                && visible.is_none_or(|visible| row.visible == visible)
                && workspace.is_none_or(|workspace| row.workspace == workspace)
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| row.id);
    match serde_json::to_string(&json!({ "windows": rows })) {
        Ok(body) => (0, Arc::from(body)),
        Err(_) => error("busy"),
    }
}

/// The whole tree, serialised at most once per snapshot and by at most one
/// reader process-wide at a time (a single-flight permit), so a
/// burst of full reads costs one serialisation and shares its bytes.
fn full_tree(snapshot: &CompSnapshot) -> Result<SerialisedReply, ()> {
    static SERIALISATION_PERMIT: Mutex<()> = Mutex::new(());
    if let Some(cached) = snapshot.full_tree.0.get() {
        return Ok(cached.clone());
    }
    let _permit = SERIALISATION_PERMIT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // A reader that waited on the permit finds the bytes its peer made.
    if let Some(cached) = snapshot.full_tree.0.get() {
        return Ok(cached.clone());
    }
    let body = serde_json::to_string(snapshot).map_err(|_| ())?;
    let bytes = body.len();
    let reply = SerialisedReply {
        body: Arc::from(body),
        bytes,
    };
    Ok(snapshot.full_tree.0.get_or_init(|| reply).clone())
}

fn enforce_reply_limit((rc, body): (u8, Arc<str>), limit_bytes: usize) -> (u8, Arc<str>) {
    if rc == 0 && body.len() > limit_bytes {
        too_large(limit_bytes)
    } else {
        (rc, body)
    }
}

fn enforce_measured_reply_limit(reply: SerialisedReply, limit_bytes: usize) -> (u8, Arc<str>) {
    if reply.bytes > limit_bytes {
        too_large(limit_bytes)
    } else {
        (0, reply.body)
    }
}

fn dispatch_selected_read(snapshot: &CompSnapshot, command: &str, args: &Value) -> (u8, Arc<str>) {
    match command {
        "comp.props.get" => match optional_path(args, "path") {
            Ok(Some(path)) => {
                let segments = path.segments().collect::<Vec<_>>();
                snapshot.select(&segments).map_or_else(
                    || error("unknown_path"),
                    |value| (0, Arc::from(value.to_string())),
                )
            }
            Ok(None) => error("busy"),
            Err(()) => error("unknown_path"),
        },
        "comp.props.list" => match optional_path(args, "prefix") {
            Ok(prefix) => {
                let leaves = snapshot.leaf_paths();
                let paths = match prefix {
                    None => leaves,
                    Some(prefix) => leaves
                        .into_iter()
                        .filter(|leaf| leaf.starts_with(&prefix))
                        .collect(),
                };
                (0, Arc::from(json!(paths).to_string()))
            }
            Err(()) => error("unknown_path"),
        },
        "comp.props.describe" => match required_path(args, "path") {
            Ok(path) => describe(snapshot, &path)
                .map_or_else(|| error("unknown_path"), |body| (0, Arc::from(body))),
            Err(()) => error("unknown_path"),
        },
        _ => error("unknown_verb"),
    }
}

fn optional_path(args: &Value, key: &str) -> Result<Option<PropPath>, ()> {
    if args.is_null() {
        return Ok(None);
    }
    let object = args.as_object().ok_or(())?;
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(path)) => PropPath::new(path.clone()).map(Some).map_err(|_| ()),
        Some(_) => Err(()),
    }
}

fn required_path(args: &Value, key: &str) -> Result<PropPath, ()> {
    optional_path(args, key)?.ok_or(())
}

/// Every leaf path of a serialised tree (objects recurse; lists are leaves).
pub fn flattened_paths(tree: &Value) -> Vec<PropPath> {
    let mut paths = Vec::new();
    flatten_into(tree, "", &mut paths);
    paths
}

fn flatten_into(value: &Value, prefix: &str, paths: &mut Vec<PropPath>) {
    if let Value::Object(object) = value {
        for (key, child) in object {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            flatten_into(child, &path, paths);
        }
    } else if let Ok(path) = PropPath::new(prefix) {
        paths.push(path);
    }
}

fn describe(snapshot: &CompSnapshot, path: &PropPath) -> Option<String> {
    let segments = path.segments().collect::<Vec<_>>();
    let node_kind = snapshot.node_kind(&segments)?;
    let matches = DESCRIPTORS
        .iter()
        .copied()
        .filter(|entry| entry.matches(path))
        .collect::<Vec<_>>();
    if node_kind == SnapshotNodeKind::Leaf {
        let [entry] = matches.as_slice() else {
            return None;
        };
        return serde_json::to_string(&DescribeReply {
            path: path.as_str(),
            ty: entry.ty.name(),
            mutable: entry.mutable,
            sensitive: entry.sensitive,
            description: entry.description,
            format: entry.format,
            enum_values: entry.enum_values,
            range: entry.range,
            persistence: entry.persistence,
            owner: entry.owner,
            children: None,
            volatile: entry.volatile,
        })
        .ok();
    }

    let leaves = snapshot.leaf_paths();
    let mut children = BTreeSet::new();
    let prefix_len = path.segments().count();
    for leaf in leaves.into_iter().filter(|leaf| leaf.starts_with(path)) {
        if let Some(child) = leaf.segments().nth(prefix_len) {
            children.insert(format!("{}.{}", path.as_str(), child));
        }
    }
    serde_json::to_string(&DescribeReply {
        path: path.as_str(),
        ty: "object",
        mutable: false,
        sensitive: false,
        description: "Compositor property subtree",
        format: None,
        enum_values: &[],
        range: None,
        persistence: None,
        owner: "comp",
        children: Some(children.into_iter().collect()),
        volatile: volatile_path(path.as_str()),
    })
    .ok()
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
