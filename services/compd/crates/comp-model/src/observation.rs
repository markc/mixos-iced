// `ObservationRecord::wire` returns a transport-free [`TopicMessage`]
// (headers + body) rather than a `BusMessage`; the header map is a BTreeMap
// exactly like BusMessage's, so header order is the same. `rfc3339_millis` is
// hand-rolled (no chrono) and produces chrono's `to_rfc3339_opts(Millis,
// true)` form for years 0000-9999.

//! The `comp.*` topics, their payloads (every one carries `event_seq`), the
//! props value type and the `comp.props.set` ingress gate.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, json};

use surfaces::StackBand;

use crate::reply::ControlReply;
use crate::snapshot::{
    BindingRowSnapshot, OutputSnapshot, SurfaceSnapshot, WindowSnapshot, WorkspaceRowSnapshot,
    volatile_path,
};

pub const PROPS_TOPIC_SUFFIX: &str = "props.changed";
pub const SURFACE_MAPPED_TOPIC_SUFFIX: &str = "surface.mapped";
pub const SURFACE_UNMAPPED_TOPIC_SUFFIX: &str = "surface.unmapped";
pub const FOCUS_TOPIC_SUFFIX: &str = "focus.changed";
pub const OUTPUT_TOPIC_SUFFIX: &str = "output.changed";
pub const CORNER_ENTERED_TOPIC_SUFFIX: &str = "corner.entered";
pub const CORNER_LEFT_TOPIC_SUFFIX: &str = "corner.left";
pub const CORNER_CLICKED_TOPIC_SUFFIX: &str = "corner.clicked";
pub const CORNER_CLICKED_V2_TOPIC_SUFFIX: &str = "corner.clicked.v2";
pub const DISCOVERY_PATH: &str = "input.corners.discovery";
pub const POINTER_TOPIC_SUFFIX: &str = "pointer.changed";
pub const PANEL_COMMAND_TOPIC_SUFFIX: &str = "panel.command";
/// The `input.corners.holders` leaf. Quoin switches to command-driven
/// reveal/conceal on this leaf, so it may only be true in a build that also
/// enforces the conceal on a stalled Quoin (holder tracking, the conceal
/// timer, enforcement, disconnect cleanup and resynchronisation). The engine
/// that serves it must keep that promise.
pub const HOLDER_PLANE_AVAILABLE: bool = true;
/// The nested backend's host-input passthrough leaf.
pub const HOST_PASSTHROUGH_PATH: &str = "input.host.passthrough";
/// The core's workspace cap.
pub const WORKSPACE_COUNT_MAX: u32 = 16;

/// Every topic suffix, in `AffectedTopics` bit order. The service publishes
/// each as `<service>.<suffix>` ([`topic_name`]).
pub const TOPIC_SUFFIXES: [&str; 11] = [
    PROPS_TOPIC_SUFFIX,
    SURFACE_MAPPED_TOPIC_SUFFIX,
    SURFACE_UNMAPPED_TOPIC_SUFFIX,
    FOCUS_TOPIC_SUFFIX,
    OUTPUT_TOPIC_SUFFIX,
    CORNER_ENTERED_TOPIC_SUFFIX,
    CORNER_LEFT_TOPIC_SUFFIX,
    CORNER_CLICKED_TOPIC_SUFFIX,
    POINTER_TOPIC_SUFFIX,
    CORNER_CLICKED_V2_TOPIC_SUFFIX,
    PANEL_COMMAND_TOPIC_SUFFIX,
];

pub fn topic_name(service: &str, suffix: &str) -> String {
    format!("{service}.{suffix}")
}

pub const PANEL_ARGS: &[&str] =
    &["output", "edge", "surface", "holder", "acquire", "mode", "generation"];

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelRequest {
    pub output: String,
    pub edge: String,
    pub surface: String,
    pub holder: Option<String>,
    pub acquire: Option<bool>,
    pub mode: Option<String>,
    /// `comp.panel.mode` only: the reporter's Bus connection generation. A
    /// report from another generation is a new Bus incarnation of the
    /// holder, whose predecessor's explicit holds must not survive.
    pub generation: Option<u64>,
    /// The broker-stamped sender (the registered service, or empty for an
    /// anonymous caller). Set by the dispatch boundary, never by the body.
    #[serde(skip)]
    pub sender: String,
}

impl PanelRequest {
    /// Refusals name the offending argument, like the window/input verbs.
    pub fn parse(verb: &str, args: &Value) -> Result<Self, ControlReply> {
        let invalid = |field: &str| ControlReply::InvalidArgs {
            field: field.to_owned(),
            allowed: PANEL_ARGS,
        };
        let Some(object) = args.as_object() else {
            return Err(invalid("args"));
        };
        if let Some(unknown) = object.keys().find(|name| !PANEL_ARGS.contains(&name.as_str())) {
            return Err(invalid(unknown.as_str()));
        }
        // A missing required field or a wrong JSON type names that field too.
        type Check = (&'static str, bool, fn(&Value) -> bool);
        let typed: [Check; 7] = [
            ("output", true, Value::is_string),
            ("edge", true, Value::is_string),
            ("surface", true, Value::is_string),
            ("holder", false, Value::is_string),
            ("acquire", false, Value::is_boolean),
            ("mode", false, Value::is_string),
            ("generation", false, Value::is_u64),
        ];
        for (name, required, ok) in typed {
            let valid = object.get(name).filter(|value| !value.is_null()).map_or(!required, ok);
            if !valid {
                return Err(invalid(name));
            }
        }
        let request: Self = serde_json::from_value(args.clone()).map_err(|_| invalid("args"))?;
        let hold = verb == "comp.panel.hold";
        let bounded = |value: &str| !value.is_empty() && value.len() <= 256;
        let checks = [
            ("output", bounded(&request.output)),
            ("surface", bounded(&request.surface)),
            ("edge", matches!(request.edge.as_str(), "top" | "bottom" | "left" | "right")),
            ("holder", if hold {
                matches!(request.holder.as_deref(), Some("pointer" | "focus" | "popup"))
            } else {
                request.holder.is_none()
            }),
            ("acquire", request.acquire.is_some() == hold),
            ("mode", if hold {
                request.mode.is_none()
            } else {
                matches!(request.mode.as_deref(), Some("hidden" | "pinned" | "docked"))
            }),
            ("generation", !hold || request.generation.is_none()),
        ];
        match checks.into_iter().find(|(_, ok)| !ok) {
            Some((field, _)) => Err(invalid(field)),
            None => Ok(request),
        }
    }
}

/// A hot corner of an output.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    pub const ALL: [Self; 4] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomRight,
    ];

    /// Position in [`Corner::ALL`].
    pub const fn index(self) -> usize {
        match self {
            Self::TopLeft => 0,
            Self::TopRight => 1,
            Self::BottomLeft => 2,
            Self::BottomRight => 3,
        }
    }

    /// The panel edge this corner's hotspot governs: the next edge
    /// counter-clockwise, named as the holder plane names it.
    pub const fn summoned_edge(self) -> &'static str {
        match self {
            Self::TopLeft => "left",
            Self::BottomLeft => "bottom",
            Self::BottomRight => "right",
            Self::TopRight => "top",
        }
    }

    /// The `corner` field of the corner topics.
    pub const fn name(self) -> &'static str {
        match self {
            Self::TopLeft => "tl",
            Self::TopRight => "tr",
            Self::BottomLeft => "bl",
            Self::BottomRight => "br",
        }
    }
}

/// The `input.corners.*` configuration leaves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CornerConfig {
    pub enabled: bool,
    /// Hotspot side in logical units.
    pub deadzone_px: f64,
    pub dwell_ms: u64,
    pub velocity_max_px_s: f64,
    /// Comp draws the hover reveal, release flash and discovery flash.
    pub affordance: bool,
    /// Slow discovery flash on every hotspot until the first reveal.
    pub discovery: bool,
}

impl Default for CornerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            deadzone_px: 10.0,
            dwell_ms: 200,
            velocity_max_px_s: 1_500.0,
            affordance: true,
            discovery: false,
        }
    }
}

impl CornerConfig {
    pub fn valid(self) -> bool {
        self.deadzone_px.is_finite()
            && (1.0..=256.0).contains(&self.deadzone_px)
            && self.dwell_ms <= 5_000
            && self.velocity_max_px_s.is_finite()
            && (1.0..=20_000.0).contains(&self.velocity_max_px_s)
    }
}

/// One `pointer.changed` sample.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PointerSample {
    pub version: u32,
    pub instance: Arc<str>,
    pub output: Option<String>,
    pub position: Option<PointerPosition>,
    pub valid: bool,
    /// Monotonic milliseconds since this compositor observation instance.
    pub timestamp_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct PointerPosition {
    pub x: f64,
    pub y: f64,
}

/// Identity fields a map edge carries, so a streaming observer can match
/// the window without a props read. Title and app id are null while a
/// session lock is active, as in the read tree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SurfaceEdgeWindow {
    pub generation: u64,
    pub app_id: Option<Arc<str>>,
    pub title: Option<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum PropValue {
    Null(()),
    Bool(bool),
    U64(u64),
    I32(i32),
    U32(u32),
    F32(f32),
    F64(f64),
    String(String),
    U64List(Vec<u64>),
    BindingRows(Vec<BindingRowSnapshot>),
    WorkspaceRows(Vec<WorkspaceRowSnapshot>),
    OutputRow(Box<OutputSnapshot>),
    SurfaceRow(Box<SurfaceSnapshot>),
    WindowRow(Box<WindowSnapshot>),
}

impl PropValue {
    pub fn null() -> Self {
        Self::Null(())
    }

    pub fn wire_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

pub fn prop_str(value: &str) -> PropValue {
    PropValue::String(value.to_string())
}

pub fn prop_opt_string(value: Option<&str>) -> PropValue {
    value.map_or_else(PropValue::null, prop_str)
}

pub fn prop_opt_u64(value: Option<u64>) -> PropValue {
    value.map_or_else(PropValue::null, PropValue::U64)
}

pub fn prop_opt_u32(value: Option<u32>) -> PropValue {
    value.map_or_else(PropValue::null, PropValue::U32)
}

#[derive(Clone, Debug, PartialEq)]
pub enum SetValidationError {
    UnknownPath,
    ReadOnly,
    InvalidValue {
        path: String,
        expected: &'static str,
        range: &'static str,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ValidatedCornerValue {
    Enabled(bool),
    DeadzonePx(f64),
    DwellMs(u64),
    VelocityMaxPxS(f64),
    Affordance(bool),
    Discovery(bool),
}

/// A `<service>.<suffix>` publication without its transport: the Bus
/// headers (a sorted map, as `bus::BusMessage` keeps them) and the
/// JSON body. The transport adds routing headers (`from`, `type`, ...).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopicMessage {
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

impl TopicMessage {
    pub fn set(&mut self, key: &str, value: &str) {
        self.headers.insert(key.to_string(), value.to_string());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.headers.get(key).map(String::as_str)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ObservationRecord {
    PanelCommand {
        output: String,
        edge: String,
        surface: String,
        reveal: bool,
        event_seq: u64,
    },
    PointerChanged {
        sample: PointerSample,
        event_seq: u64,
    },
    PropsChanged {
        path: String,
        old: PropValue,
        new: PropValue,
        unix_ms: i64,
        cause: &'static str,
        event_seq: u64,
    },
    SurfaceMapped {
        id: u64,
        role: String,
        foreign_id: Option<String>,
        window: SurfaceEdgeWindow,
        event_seq: u64,
    },
    SurfaceUnmapped {
        id: u64,
        role: String,
        foreign_id: Option<String>,
        window: SurfaceEdgeWindow,
        event_seq: u64,
    },
    FocusChanged {
        keyboard: Option<u64>,
        previous: Option<u64>,
        exclusive_latch: Option<u64>,
        event_seq: u64,
    },
    OutputChanged {
        output: String,
        row: OutputSnapshot,
        event_seq: u64,
    },
    CornerEntered {
        output: String,
        corner: Corner,
        dwell_ms: u64,
        event_seq: u64,
    },
    CornerLeft {
        output: String,
        corner: Corner,
        dwell_ms: u64,
        event_seq: u64,
    },
    CornerClicked {
        output: String,
        corner: Corner,
        dwell_ms: u64,
        event_seq: u64,
    },
    CornerClickedV2 {
        output: String,
        corner: Corner,
        dwell_ms: u64,
        button: &'static str,
        kind: &'static str,
        modifiers: Vec<&'static str>,
        event_seq: u64,
    },
}

impl ObservationRecord {
    pub fn event_seq(&self) -> u64 {
        match self {
            Self::PanelCommand { event_seq, .. } => *event_seq,
            Self::PointerChanged { event_seq, .. } => *event_seq,
            Self::PropsChanged { event_seq, .. }
            | Self::SurfaceMapped { event_seq, .. }
            | Self::SurfaceUnmapped { event_seq, .. }
            | Self::FocusChanged { event_seq, .. }
            | Self::OutputChanged { event_seq, .. }
            | Self::CornerEntered { event_seq, .. }
            | Self::CornerClicked { event_seq, .. }
            | Self::CornerClickedV2 { event_seq, .. }
            | Self::CornerLeft { event_seq, .. } => *event_seq,
        }
    }

    /// The same record numbered `seq`: an engine builds its records first and
    /// the outbox numbers each one as it is offered (`offer_next`).
    pub fn with_event_seq(mut self, seq: u64) -> Self {
        match &mut self {
            Self::PanelCommand { event_seq, .. }
            | Self::PointerChanged { event_seq, .. }
            | Self::PropsChanged { event_seq, .. }
            | Self::SurfaceMapped { event_seq, .. }
            | Self::SurfaceUnmapped { event_seq, .. }
            | Self::FocusChanged { event_seq, .. }
            | Self::OutputChanged { event_seq, .. }
            | Self::CornerEntered { event_seq, .. }
            | Self::CornerClicked { event_seq, .. }
            | Self::CornerClickedV2 { event_seq, .. }
            | Self::CornerLeft { event_seq, .. } => *event_seq = seq,
        }
        self
    }

    pub fn topic_suffix(&self) -> &'static str {
        match self {
            Self::PanelCommand { .. } => PANEL_COMMAND_TOPIC_SUFFIX,
            Self::PointerChanged { .. } => POINTER_TOPIC_SUFFIX,
            Self::PropsChanged { .. } => PROPS_TOPIC_SUFFIX,
            Self::SurfaceMapped { .. } => SURFACE_MAPPED_TOPIC_SUFFIX,
            Self::SurfaceUnmapped { .. } => SURFACE_UNMAPPED_TOPIC_SUFFIX,
            Self::FocusChanged { .. } => FOCUS_TOPIC_SUFFIX,
            Self::OutputChanged { .. } => OUTPUT_TOPIC_SUFFIX,
            Self::CornerEntered { .. } => CORNER_ENTERED_TOPIC_SUFFIX,
            Self::CornerLeft { .. } => CORNER_LEFT_TOPIC_SUFFIX,
            Self::CornerClicked { .. } => CORNER_CLICKED_TOPIC_SUFFIX,
            Self::CornerClickedV2 { .. } => CORNER_CLICKED_V2_TOPIC_SUFFIX,
        }
    }

    /// The publication: `command` is the unprefixed topic suffix,
    /// `event_seq` a header and a body field; `props.changed` also carries
    /// `path` and `cause` headers.
    pub fn wire(&self) -> TopicMessage {
        let mut message = TopicMessage::default();
        message.set("command", self.topic_suffix());
        message.set("event_seq", &self.event_seq().to_string());
        message.body = match self {
            Self::PanelCommand { output, edge, surface, reveal, event_seq } => json!({
                "version": 1, "output": output, "edge": edge, "surface": surface,
                "action": if *reveal { "reveal" } else { "conceal" }, "event_seq": event_seq,
            }).to_string(),
            Self::CornerClickedV2 {
                output,
                corner,
                dwell_ms,
                button,
                kind,
                modifiers,
                event_seq,
            } => json!({
                "output": output,
                "corner": corner.name(),
                "dwell_ms": dwell_ms,
                "button": button,
                "kind": kind,
                "modifiers": modifiers,
                "event_seq": event_seq,
            })
            .to_string(),
            Self::PointerChanged { sample, event_seq } => {
                let mut value = serde_json::to_value(sample).expect("finite pointer sample");
                value["event_seq"] = json!(event_seq);
                value.to_string()
            }
            Self::PropsChanged {
                path,
                old,
                new,
                unix_ms,
                cause,
                event_seq,
            } => {
                message.set("path", path);
                message.set("cause", cause);
                json!({
                    "path": path,
                    "old": old.wire_value(),
                    "new": new.wire_value(),
                    "ts": rfc3339_millis(*unix_ms),
                    "cause": cause,
                    "event_seq": event_seq,
                })
                .to_string()
            }
            Self::SurfaceMapped {
                id,
                role,
                foreign_id,
                window,
                event_seq,
            }
            | Self::SurfaceUnmapped {
                id,
                role,
                foreign_id,
                window,
                event_seq,
            } => {
                let mut body = json!({
                    "id": id,
                    "role": role,
                    "generation": window.generation,
                    "app_id": window.app_id.as_deref(),
                    "title": window.title.as_deref(),
                    "event_seq": event_seq,
                });
                if let Some(foreign_id) = foreign_id {
                    body.as_object_mut()
                        .expect("surface event body is an object")
                        .insert("foreign_id".into(), json!(foreign_id));
                }
                body.to_string()
            }
            Self::FocusChanged {
                keyboard,
                previous,
                exclusive_latch,
                event_seq,
            } => json!({
                "keyboard": keyboard,
                "previous": previous,
                "exclusive_latch": exclusive_latch,
                "event_seq": event_seq,
            })
            .to_string(),
            Self::OutputChanged {
                output,
                row,
                event_seq,
            } => json!({
                "output": output,
                "geometry": {
                    "x": row.x,
                    "y": row.y,
                    "width": row.width,
                    "height": row.height,
                },
                "usable": row.usable,
                "event_seq": event_seq,
            })
            .to_string(),
            Self::CornerEntered {
                output,
                corner,
                dwell_ms,
                event_seq,
            }
            | Self::CornerLeft {
                output,
                corner,
                dwell_ms,
                event_seq,
            }
            | Self::CornerClicked {
                output,
                corner,
                dwell_ms,
                event_seq,
            } => json!({
                "output": output,
                "corner": corner.name(),
                "dwell_ms": dwell_ms,
                "event_seq": event_seq,
            })
            .to_string(),
        };
        message
    }
}

/// The set of topics a loss interval touched, one bit per
/// [`TOPIC_SUFFIXES`] entry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AffectedTopics(u16);

impl AffectedTopics {
    fn index(suffix: &str) -> usize {
        TOPIC_SUFFIXES
            .iter()
            .position(|candidate| *candidate == suffix)
            .expect("every observation has a fixed topic suffix")
    }

    pub fn insert(&mut self, suffix: &str) {
        let index = Self::index(suffix);
        self.0 |= 1 << index;
    }

    pub fn merge(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, suffix: &str) {
        self.0 &= !(1 << Self::index(suffix));
    }

    pub fn contains(self, suffix: &str) -> bool {
        self.0 & (1 << Self::index(suffix)) != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn bits(self) -> u16 {
        self.0
    }

    pub fn iter(self) -> impl Iterator<Item = &'static str> {
        TOPIC_SUFFIXES
            .into_iter()
            .enumerate()
            .filter_map(move |(index, suffix)| (self.0 & (1 << index) != 0).then_some(suffix))
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LossCause {
    OutboxOverflow,
    PublisherLoss,
}

impl LossCause {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OutboxOverflow => "outbox.overflow",
            Self::PublisherLoss => "publisher.loss",
        }
    }
}

/// A run of event sequences that never reached the Bus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LossInterval {
    pub first_lost_seq: u64,
    pub last_lost_seq: u64,
    pub topics: AffectedTopics,
    pub cause: LossCause,
}

impl LossInterval {
    pub fn from_record(record: &ObservationRecord, cause: LossCause) -> Self {
        let mut topics = AffectedTopics::default();
        topics.insert(record.topic_suffix());
        Self {
            first_lost_seq: record.event_seq(),
            last_lost_seq: record.event_seq(),
            topics,
            cause,
        }
    }

    pub fn merge(&mut self, other: Self) {
        self.first_lost_seq = self.first_lost_seq.min(other.first_lost_seq);
        self.last_lost_seq = self.last_lost_seq.max(other.last_lost_seq);
        self.topics.merge(other.topics);
        self.cause = self.cause.max(other.cause);
    }
}

/// The gap publication on one topic: `event_seq` names the last lost
/// sequence, the body says how many were lost and why.
pub fn gap_message(topic_suffix: &str, gap: LossInterval, lost_count: u64) -> TopicMessage {
    let mut message = TopicMessage::default();
    message.set("command", topic_suffix);
    message.set("event_seq", &gap.last_lost_seq.to_string());
    message.body = json!({
        "gap": true,
        "lost_count": lost_count,
        "cause": gap.cause.as_str(),
    })
    .to_string();
    message
}

/// The one event-sequence counter every topic shares. Sequences start at 1,
/// increase by one, and stop for good at `u64::MAX` (offered once): a
/// sequence never repeats.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EventSeq {
    last: u64,
    exhausted: bool,
}

impl EventSeq {
    /// A counter whose last handed-out sequence is `last` (0 = none yet).
    pub fn starting_after(last: u64) -> Self {
        Self {
            last,
            exhausted: last == u64::MAX,
        }
    }

    pub fn next_seq(&mut self) -> Option<u64> {
        if self.exhausted {
            return None;
        }
        self.last += 1;
        self.exhausted = self.last == u64::MAX;
        Some(self.last)
    }

    /// The last sequence handed out (`port.event_seq`, the watermark).
    pub fn current(self) -> u64 {
        self.last
    }

    pub fn exhausted(self) -> bool {
        self.exhausted
    }
}

/// UTC milliseconds as RFC 3339 with millisecond precision and `Z`, as
/// chrono's `to_rfc3339_opts(SecondsFormat::Millis, true)` writes it.
pub fn rfc3339_millis(unix_ms: i64) -> String {
    let seconds = unix_ms.div_euclid(1000);
    let millis = unix_ms.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        second_of_day / 3_600,
        second_of_day % 3_600 / 60,
        second_of_day % 60,
    )
}

/// Pending `props.changed` records for one diff pass, keyed by path:
/// `(old, new, cause)`.
pub type PendingPropChanges = BTreeMap<String, (PropValue, PropValue, &'static str)>;

/// Queue one leaf change. A path changed twice in one pass keeps its first
/// `old` and cause and its last `new`; a change that returns to `old` is
/// dropped, and `port.*` and volatile paths never publish.
pub fn queue_prop_change(
    pending: &mut PendingPropChanges,
    path: String,
    old: PropValue,
    new: PropValue,
    cause: &'static str,
) {
    if old == new || path.starts_with("port.") || volatile_path(&path) {
        return;
    }
    match pending.entry(path) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert((old, new, cause));
        }
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            entry.get_mut().1 = new;
            if entry.get().0 == entry.get().1 {
                entry.remove();
            }
        }
    }
}

/// Whether one change may become a `props.changed` record at all.
pub fn publishes_prop_change(path: &str, old: &PropValue, new: &PropValue) -> bool {
    !(old == new || path.starts_with("port.") || volatile_path(path))
}

/// The `range` a workspace index refusal reports; the live bound is the
/// count, which the core checks.
pub const WORKSPACE_INDEX_RANGE: &str = "1..=count";
/// The `range` a count refusal reports — the core's `WORKSPACE_COUNT_MAX`.
pub const WORKSPACE_COUNT_RANGE: &str = "1..=16";
/// The `range` an output-key refusal reports: only the default output's
/// current workspace is switchable.
pub const WORKSPACE_OUTPUT_RANGE: &str =
    "the default output's o_<slug> (the only switchable output; workspaces.current addresses it)";
const _: () = assert!(
    WORKSPACE_COUNT_MAX == 16,
    "WORKSPACE_COUNT_RANGE names the core's cap"
);

/// Where a `workspaces.*` write is aimed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspacesSetTarget {
    Count,
    /// `workspaces.current` (`None`: the default output) or
    /// `workspaces.o_<slug>.current` (`Some(key)`).
    Current(Option<String>),
}

/// Parse the three writable `workspaces.*` shapes. Everything else under
/// the subtree is read-only (`workspaces`, `workspaces.list`) or unknown.
pub fn parse_workspaces_set_path(path: &str) -> Option<WorkspacesSetTarget> {
    match path {
        "workspaces.count" => Some(WorkspacesSetTarget::Count),
        "workspaces.current" => Some(WorkspacesSetTarget::Current(None)),
        _ => {
            let key = path.strip_prefix("workspaces.")?.strip_suffix(".current")?;
            (key.starts_with("o_") && key.len() > 2 && !key.contains('.'))
                .then(|| WorkspacesSetTarget::Current(Some(key.to_string())))
        }
    }
}

/// A workspace index or count on the wire: an unsigned integer >= 1 that
/// fits a `u32`. The live upper bound is the core's check, reported through
/// the same `range`.
pub fn workspace_value(
    path: &str,
    value: &Value,
    range: &'static str,
) -> Result<u32, SetValidationError> {
    value
        .as_u64()
        .filter(|value| *value >= 1)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid_value(path, "integer", range))
}

/// Parse a `windows.s<id>.band` write path into the window's surface id.
pub fn parse_window_band_path(path: &str) -> Option<u64> {
    parse_window_leaf_path(path).and_then(|(id, leaf)| (leaf == "band").then_some(id))
}

/// Parse `windows.s<id>.<leaf>` into the canonical id and the one-segment
/// leaf name. Used by the write gate and the `generation` fence.
pub fn parse_window_leaf_path(path: &str) -> Option<(u64, &str)> {
    let (id, leaf) = path.strip_prefix("windows.s")?.split_once('.')?;
    if leaf.is_empty() || leaf.contains('.') {
        return None;
    }
    // Canonical ids only: a leading zero ("windows.s0007.band") would write
    // through an alias that reads, describes and event-diffs as "s7".
    if id.is_empty()
        || (id.len() > 1 && id.starts_with('0'))
        || !id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some((id.parse().ok()?, leaf))
}

/// A window band write accepts exactly the two operator-reachable bands:
/// `bottom` and `normal`. The other bands belong to layer-shell and
/// session-lock surfaces, never to toplevels.
pub fn validate_window_band_value(path: &str, value: &Value) -> Result<StackBand, SetValidationError> {
    match value.as_str() {
        Some("bottom") => Ok(StackBand::Bottom),
        Some("normal") => Ok(StackBand::Normal),
        _ => Err(invalid_value(path, "string", "bottom|normal")),
    }
}

/// The one ingress-side gate for `comp.props.set`: admits every writable
/// leaf family. Value validation is repeated by the service arms;
/// state-dependent checks (does the window exist) belong to the service.
pub fn validate_set_request(path: &str, value: &Value) -> Result<(), SetValidationError> {
    #[cfg(feature = "xwayland")]
    if path == "xwayland.enabled" {
        return if value.is_boolean() {
            Ok(())
        } else {
            Err(invalid_value(path, "bool", "true|false"))
        };
    }
    if parse_window_band_path(path).is_some() {
        return validate_window_band_value(path, value).map(|_| ());
    }
    if path == HOST_PASSTHROUGH_PATH {
        // Backend presence is the service's call (the leaf exists only on
        // the nested backend).
        return if value.is_boolean() {
            Ok(())
        } else {
            Err(invalid_value(path, "bool", "true|false"))
        };
    }
    if parse_window_leaf_path(path).is_some_and(|(_, leaf)| matches!(leaf, "minimized" | "maximized" | "fullscreen")) {
        return if value.is_boolean() {
            Ok(())
        } else {
            Err(invalid_value(path, "bool", "true|false"))
        };
    }
    // Workspace leaves: an integer >= 1 passes the gate; the live upper
    // bound (the count, or the cap) is the service's, reported through
    // the same range strings.
    if parse_window_leaf_path(path).is_some_and(|(_, leaf)| leaf == "workspace") {
        return workspace_value(path, value, WORKSPACE_INDEX_RANGE).map(|_| ());
    }
    match parse_workspaces_set_path(path) {
        Some(WorkspacesSetTarget::Count) => {
            return workspace_value(path, value, WORKSPACE_COUNT_RANGE).map(|_| ());
        }
        Some(WorkspacesSetTarget::Current(_)) => {
            return workspace_value(path, value, WORKSPACE_INDEX_RANGE).map(|_| ());
        }
        None => {}
    }
    validate_corner_value(path, value).map(|_| ())
}

pub fn validate_corner_value(
    path: &str,
    value: &Value,
) -> Result<ValidatedCornerValue, SetValidationError> {
    match path {
        "input.corners.holders" => Err(SetValidationError::ReadOnly),
        _ if ["input.corners.enforced", "input.corners.held"].iter().any(|counts| {
            path == *counts
                || path.strip_prefix(*counts).and_then(|rest| rest.strip_prefix('.')).is_some_and(
                    |edge| matches!(edge, "top" | "bottom" | "left" | "right"),
                )
        }) =>
        {
            Err(SetValidationError::ReadOnly)
        }
        "input.corners.enabled" => {
            let Some(value) = value.as_bool() else {
                return Err(invalid_value(path, "bool", "true|false"));
            };
            Ok(ValidatedCornerValue::Enabled(value))
        }
        "input.corners.deadzone_px" => {
            let value = finite_number(path, value, "finite number", "1.0..=256.0")?;
            if !(1.0..=256.0).contains(&value) {
                return Err(invalid_value(path, "finite number", "1.0..=256.0"));
            }
            Ok(ValidatedCornerValue::DeadzonePx(value))
        }
        "input.corners.dwell_ms" => {
            let Some(value) = value.as_u64().filter(|value| *value <= 5_000) else {
                return Err(invalid_value(path, "integer", "0..=5000"));
            };
            Ok(ValidatedCornerValue::DwellMs(value))
        }
        "input.corners.velocity_max_px_s" => {
            let value = finite_number(path, value, "finite number", "1.0..=20000.0")?;
            if !(1.0..=20_000.0).contains(&value) {
                return Err(invalid_value(path, "finite number", "1.0..=20000.0"));
            }
            Ok(ValidatedCornerValue::VelocityMaxPxS(value))
        }
        "input.corners.affordance" => {
            let Some(value) = value.as_bool() else {
                return Err(invalid_value(path, "bool", "true|false"));
            };
            Ok(ValidatedCornerValue::Affordance(value))
        }
        DISCOVERY_PATH => {
            let Some(value) = value.as_bool() else {
                return Err(invalid_value(path, "bool", "true|false"));
            };
            Ok(ValidatedCornerValue::Discovery(value))
        }
        _ if path.starts_with("input.corners.") => Err(SetValidationError::UnknownPath),
        _ if known_read_only_path(path) => Err(SetValidationError::ReadOnly),
        _ => Err(SetValidationError::UnknownPath),
    }
}

/// Apply a validated corner value; returns `(old, new)` for the change
/// record.
pub fn apply_corner_value(
    config: &mut CornerConfig,
    value: ValidatedCornerValue,
) -> (PropValue, PropValue) {
    match value {
        ValidatedCornerValue::Enabled(value) => {
            let old = config.enabled;
            config.enabled = value;
            (PropValue::Bool(old), PropValue::Bool(value))
        }
        ValidatedCornerValue::DeadzonePx(value) => {
            let old = config.deadzone_px;
            config.deadzone_px = value;
            (PropValue::F64(old), PropValue::F64(value))
        }
        ValidatedCornerValue::DwellMs(value) => {
            let old = config.dwell_ms;
            config.dwell_ms = value;
            (PropValue::U64(old), PropValue::U64(value))
        }
        ValidatedCornerValue::VelocityMaxPxS(value) => {
            let old = config.velocity_max_px_s;
            config.velocity_max_px_s = value;
            (PropValue::F64(old), PropValue::F64(value))
        }
        ValidatedCornerValue::Affordance(value) => {
            let old = config.affordance;
            config.affordance = value;
            (PropValue::Bool(old), PropValue::Bool(value))
        }
        ValidatedCornerValue::Discovery(value) => {
            let old = config.discovery;
            config.discovery = value;
            (PropValue::Bool(old), PropValue::Bool(value))
        }
    }
}

fn finite_number(
    path: &str,
    value: &Value,
    expected: &'static str,
    range: &'static str,
) -> Result<f64, SetValidationError> {
    value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or_else(|| invalid_value(path, expected, range))
}

pub fn invalid_value(path: &str, expected: &'static str, range: &'static str) -> SetValidationError {
    SetValidationError::InvalidValue {
        path: path.to_string(),
        expected,
        range,
    }
}

/// Unknown and read-only window leaves keep the ordinary set errors.
pub fn read_only_or_unknown(path: &str) -> SetValidationError {
    if known_read_only_path(path) {
        SetValidationError::ReadOnly
    } else {
        SetValidationError::UnknownPath
    }
}

/// A served path that `comp.props.set` refuses as `read_only` rather than
/// `unknown_path`.
pub fn known_read_only_path(path: &str) -> bool {
    const ROOTS: &[&str] = &[
        "info",
        "outputs",
        "surfaces",
        "windows",
        "stack",
        "focus",
        "decoration",
        "bindings",
        "dmabuf",
        "port",
    ];
    #[cfg(feature = "xwayland")]
    if path == "xwayland"
        || path == "xwayland.persist_path"
        || path == "xwayland.display"
        || path == "xwayland.state"
        || path == "xwayland.failures"
    {
        // The subtree object is read-only like "input", and so are its four
        // served-but-never-written leaves; the one writable leaf is routed
        // before validation ever runs.
        return true;
    }
    // The subtree object and the row list; the three writable leaves are
    // routed before validation ever reaches here, and any other
    // `workspaces.*` spelling is unknown, not read-only.
    if path == "workspaces" || path == "workspaces.list" {
        return true;
    }
    path == "input"
        || path == "input.last_origin"
        || path == "input.seats"
        || path.starts_with("input.seats.")
        || path == "input.corners"
        || path == "input.host"
        || ROOTS
            .iter()
            .any(|root| path == *root || path.starts_with(&format!("{root}.")))
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;
