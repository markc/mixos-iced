// The request types (without the transport's oneshot-carrying `Port*Request`
// structs), the argument parsers and the dispatcher's routing order, lifted
// into [`classify`]. `SeatKind` comes from surfaces; the region cleanup
// budget is a local constant; `LongOp::budget` and the agent-seat predicates
// of `PortControl` are public.

//! `comp.*` request routing and argument parsing. Each parser refuses an
//! argument the verb does not define by name (`invalid_args`), so a typo
//! such as `{"gen": 3}` can never act unfenced, and parses `{id,
//! generation}` strictly.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use ledger::presentation_stats::STATS_RING;
use surfaces::SeatKind;

use crate::catalogue::{INPUT_VERBS, READ_VERBS, WINDOW_VERBS};
use crate::observation::{
    PanelRequest, SetValidationError, parse_window_leaf_path, validate_set_request,
};
use crate::reply::{ControlReply, error};

/// The longest a long verb may run before its reply is due.
pub const LONG_VERB_MAX: Duration = Duration::from_secs(60);
/// Admission slack on top of a long verb's own deadline: the compositor
/// answers at the deadline, and the worker must still be listening.
pub const LONG_VERB_SLACK: Duration = Duration::from_secs(1);
pub const SEQUENCE_MAX_STEPS: usize = 256;
/// At most four events a character (Shift press, key press/release,
/// Shift release), so the largest text stays within one verb's event cap.
pub const TEXT_MAX_CHARS: usize = 256;
/// The most seat events one verb (a whole sequence included) may inject.
pub const MAX_EVENTS_PER_VERB: usize = 4096;
/// How long a finished region selection may take to clean up its overlay
/// before the reply.
pub const REGION_CLEANUP_BUDGET: Duration = Duration::from_secs(3);

/// What `comp.window.stats` / `.stats.reset` measure: a window, fenced by
/// its role generation, or a content source, optionally fenced by its
/// registration number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatsTarget {
    Window {
        id: u64,
        generation: u64,
    },
    Source {
        id: String,
        registration: Option<u64>,
    },
}

/// A window-addressed verb. `{id, generation}` is always required when a
/// window is named; only `restore` may name none (most recently minimised).
#[derive(Clone, Debug, PartialEq)]
pub enum WindowOp {
    HardwareSnapshot,
    WorldList,
    WorldCreate,
    WorldActivate {
        id: uuid::Uuid,
    },
    Tile {
        id: u64,
        generation: u64,
        enabled: bool,
        output: Option<String>,
    },
    State {
        id: u64,
        generation: u64,
        state: WindowState,
        enabled: bool,
        output: Option<String>,
    },
    Minimize {
        id: u64,
        generation: u64,
    },
    Restore {
        target: Option<(u64, u64)>,
    },
    /// Keyboard focus; `raise` also raises and retargets the pointer (the
    /// Alt+Tab activation).
    Focus {
        id: u64,
        generation: u64,
        raise: bool,
    },
    Raise {
        id: u64,
        generation: u64,
    },
    /// The polite close (xdg `close` / X11 `WM_DELETE_WINDOW`).
    Close {
        id: u64,
        generation: u64,
    },
    Place(PlaceSpec),
    /// Presentation statistics for one window or content source.
    Stats {
        target: StatsTarget,
        samples: usize,
    },
    /// Zero one window's, one source's, or every row's statistics.
    StatsReset {
        target: Option<StatsTarget>,
    },
    /// `comp.workspace.switch`: the output's current workspace (`None` =
    /// the default output). Names no window, but changes what is on
    /// screen, so a session lock refuses it like the rest.
    SwitchWorkspace {
        output: Option<String>,
        index: WorkspaceIndex,
        wrap: bool,
    },
    /// `comp.window.send_to_workspace`: move one window; `follow` also
    /// switches to it and activates the window.
    SendToWorkspace {
        id: u64,
        generation: u64,
        index: WorkspaceIndex,
        follow: bool,
    },
}

/// Where `comp.workspace.switch` / `send_to_workspace` aim: a 1-based
/// index, or one step relative to the current (switch) or the window's own
/// (send) workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceIndex {
    Absolute(u32),
    Next,
    Prev,
}

/// `comp.window.place`: output-local logical window-geometry coordinates.
/// An absent field keeps its current value.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaceSpec {
    pub id: u64,
    pub generation: u64,
    pub output: Option<String>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowState {
    Maximized,
    Fullscreen,
}

/// What `comp.window.wait` waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitUntil {
    Tiled,
    Untiled,
    Maximized,
    Unmaximized,
    Fullscreen,
    Unfullscreen,
    Mapped,
    Visible,
    Presented,
    Size { width: i32, height: i32 },
    Focused,
    Unmapped,
    Gone,
}

impl WaitUntil {
    pub fn name(self) -> &'static str {
        match self {
            Self::Tiled => "tiled",
            Self::Untiled => "untiled",
            Self::Maximized => "maximized",
            Self::Unmaximized => "unmaximized",
            Self::Fullscreen => "fullscreen",
            Self::Unfullscreen => "unfullscreen",
            Self::Mapped => "mapped",
            Self::Visible => "visible",
            Self::Presented => "presented",
            Self::Size { .. } => "size",
            Self::Focused => "focused",
            Self::Unmapped => "unmapped",
            Self::Gone => "gone",
        }
    }
}

/// Which window a wait is about: one `{id, generation?}`, or the first
/// window matching the name filters.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowMatch {
    pub id: Option<u64>,
    pub generation: Option<u64>,
    pub app_id: Option<String>,
    pub title: Option<String>,
    pub title_contains: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WaitSpec {
    pub window: WindowMatch,
    pub until: WaitUntil,
    pub timeout: Duration,
}

/// A parsed `comp.window.*` verb: answered in one pass, or long.
#[derive(Clone, Debug, PartialEq)]
pub enum WindowVerb {
    Op(WindowOp),
    Long(LongOp),
}

/// Press, release, or both in one verb (`click` for buttons, `tap` for
/// keys).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PressAction {
    Press,
    Release,
    Both,
}

/// Where `comp.input.pointer.move` puts the pointer.
#[derive(Clone, Debug, PartialEq)]
pub enum PointerMoveTarget {
    /// Output-local logical coordinates; `None` is the default output.
    Output {
        output: Option<String>,
        x: f64,
        y: f64,
    },
    /// A relative device delta (accelerated == unaccelerated).
    Relative { dx: f64, dy: f64 },
    /// Window-local coordinates, relative to the window-geometry origin.
    Window {
        id: u64,
        generation: u64,
        x: f64,
        y: f64,
        require_hit: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollSource {
    Wheel,
    Finger,
    Continuous,
}

/// A key named by XKB keysym (`"Return"`, `"a"`, `"Super_L"`) or by raw
/// evdev code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeySpec {
    Name(String),
    Evdev(u32),
}

/// One `comp.input.*` operation, parsed and bounded on the worker.
#[derive(Clone, Debug, PartialEq)]
pub enum InputOp {
    OnSeat {
        seat: SeatKind,
        op: Box<InputOp>,
    },
    /// Focus and inject in one compositor-thread dispatch.
    Targeted {
        id: u64,
        generation: u64,
        raise: bool,
        op: Box<InputOp>,
    },
    /// `corners: false` keeps the move from arming a hot corner.
    PointerMove {
        target: PointerMoveTarget,
        corners: bool,
    },
    PointerButton {
        button: u32,
        action: PressAction,
    },
    PointerScroll {
        dx: Option<f64>,
        dy: Option<f64>,
        source: ScrollSource,
        v120: (Option<i32>, Option<i32>),
    },
    Key {
        key: KeySpec,
        action: PressAction,
        /// Keysym names of modifiers held around the key.
        modifiers: Vec<KeySpec>,
    },
    Text(String),
    ReleaseAll,
}

impl InputOp {
    /// The most seat events this op can inject. `release_all` releases what
    /// is held, which earlier (capped) verbs bounded.
    pub fn event_bound(&self) -> usize {
        match self {
            Self::OnSeat { op, .. } => op.event_bound(),
            Self::Targeted { op, .. } => {
                op.event_bound() + usize::from(matches!(op.as_ref(), Self::PointerButton { .. }))
            }
            Self::PointerMove { .. } | Self::PointerScroll { .. } | Self::ReleaseAll => 1,
            Self::PointerButton { .. } => 2,
            Self::Key { modifiers, .. } => 2 * (modifiers.len() + 2),
            Self::Text(text) => 4 * text.chars().count(),
        }
    }

    /// Whether this op drives the agent seat: such admissions are refused
    /// when the agent epoch moved on.
    pub fn uses_agent(&self) -> bool {
        matches!(
            self,
            Self::OnSeat {
                seat: SeatKind::Agent,
                ..
            }
        )
    }
}

/// One step of `comp.input.sequence`: the delay runs before the step.
#[derive(Clone, Debug, PartialEq)]
pub struct SequenceStep {
    pub verb: &'static str,
    pub op: InputOp,
    pub delay: Duration,
}

/// A verb whose reply waits on a timer or an edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardwareUntil {
    Keyboard,
    Pointer,
    Paused,
    Active,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HardwareWaitSpec {
    pub instance: String,
    pub after: u64,
    pub until: HardwareUntil,
    pub timeout: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LongOp {
    HardwareWait(HardwareWaitSpec),
    CaptureFrame(crate::capture::CaptureFrameSpec),
    RegionSelect {
        output: Option<String>,
        timeout: Duration,
        /// The optional caller identity (`selection`); legacy selections
        /// have none and keep their legacy reply shape.
        selection: Option<SelectionIdentity>,
    },
    Sequence(Vec<SequenceStep>),
    SeatedSequence {
        seat: SeatKind,
        steps: Vec<SequenceStep>,
    },
    Wait(WaitSpec),
    /// Polite close now; if the same `{id, generation}` is still alive at
    /// the deadline, kill its client.
    ForceClose {
        id: u64,
        generation: u64,
        timeout: Duration,
    },
}

impl LongOp {
    /// When the compositor must have answered by.
    pub fn budget(&self) -> Duration {
        match self {
            Self::CaptureFrame(_) => Duration::from_secs(3),
            // Reserve four seconds after interaction for an acknowledged clean
            // frame (55s + 4s = 59s), strictly inside LONG_VERB_MAX's 60s.
            Self::RegionSelect { timeout, .. } => {
                *timeout + REGION_CLEANUP_BUDGET + Duration::from_secs(1)
            }
            Self::Sequence(steps) | Self::SeatedSequence { steps, .. } => {
                steps.iter().map(|step| step.delay).sum()
            }
            Self::Wait(spec) => spec.timeout,
            Self::HardwareWait(spec) => spec.timeout,
            Self::ForceClose { timeout, .. } => *timeout,
        }
    }

    /// How long the transport waits for the compositor's answer: the
    /// verb's own deadline (capped at [`LONG_VERB_MAX`]) plus the slack.
    pub fn admission_timeout(&self) -> Duration {
        self.budget().min(LONG_VERB_MAX) + LONG_VERB_SLACK
    }

    /// Whether any step drives the agent seat.
    pub fn uses_agent(&self) -> bool {
        match self {
            Self::Sequence(steps) | Self::SeatedSequence { steps, .. } => {
                steps.iter().any(|step| step.op.uses_agent())
            }
            _ => false,
        }
    }
}

/// The refusal an agent-seat admission gets when a human input cleared the
/// agent epoch while it waited.
pub fn input_cleared_reply() -> ControlReply {
    ControlReply::refused("input_cleared", json!({"seat":"agent", "released":true}))
}

/// `comp.props.set`'s `(path, value, generation fence)`.
pub type ParsedSet = (String, Value, Option<u64>);

/// `comp.props.set {path, value, generation?}`. The fence is accepted only
/// on `windows.s<id>.*` leaves.
pub fn parse_set(args: &Value) -> Result<ParsedSet, (u8, Arc<str>)> {
    let object = args.as_object().ok_or_else(|| invalid_set_shape(None))?;
    let path = object
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| error("unknown_path"))?;
    let value = object
        .get("value")
        .cloned()
        .ok_or_else(|| invalid_set_shape(Some(path)))?;
    let generation = match object.get("generation") {
        None | Some(Value::Null) => None,
        Some(generation) => Some(generation.as_u64().ok_or_else(|| {
            invalid_argument("generation", "unsigned integer", "windows.s<id>.generation")
                .into_wire()
        })?),
    };
    // The fence names a window, so it only means something on a window
    // leaf; anywhere else it is a mis-aimed request, not something to drop.
    if generation.is_some() && parse_window_leaf_path(path).is_none() {
        return Err(invalid_argument(
            "generation",
            "absent",
            "generation applies to windows.s<id>.* paths only",
        )
        .into_wire());
    }
    Ok((path.to_string(), value, generation))
}

/// An `invalid_value` refusal naming the argument.
pub fn invalid_argument(path: &str, expected: &'static str, range: &'static str) -> ControlReply {
    ControlReply::Validation(SetValidationError::InvalidValue {
        path: path.to_string(),
        expected,
        range,
    })
}

fn window_arg(
    object: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<Option<u64>, ControlReply> {
    match object.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| invalid_argument(name, "unsigned integer", "0..=u64::MAX")),
    }
}

/// `comp.window.minimize {id, generation}` and
/// `comp.window.restore {id?, generation?}` (both or neither).
pub fn parse_window_op(verb: &str, args: &Value) -> Result<WindowOp, ControlReply> {
    let empty = serde_json::Map::new();
    let object = match args {
        Value::Null => &empty,
        Value::Object(object) => object,
        _ => {
            return Err(invalid_argument("args", "JSON object", "{id, generation}"));
        }
    };
    const WINDOW_ARGS: &[&str] = &["id", "generation"];
    if let Some(field) = object
        .keys()
        .find(|field| !WINDOW_ARGS.contains(&field.as_str()))
    {
        return Err(ControlReply::InvalidArgs {
            field: field.clone(),
            allowed: WINDOW_ARGS,
        });
    }
    let id = window_arg(object, "id")?;
    let generation = window_arg(object, "generation")?;
    let target = match (id, generation) {
        (Some(id), Some(generation)) => Some((id, generation)),
        (None, None) => None,
        (Some(_), None) => {
            return Err(invalid_argument(
                "generation",
                "unsigned integer",
                "required with id (read windows.s<id>.generation)",
            ));
        }
        (None, Some(_)) => {
            return Err(invalid_argument(
                "id",
                "unsigned integer",
                "required with generation",
            ));
        }
    };
    if verb == "comp.window.minimize" {
        let Some((id, generation)) = target else {
            return Err(invalid_argument("id", "unsigned integer", "required"));
        };
        Ok(WindowOp::Minimize { id, generation })
    } else {
        Ok(WindowOp::Restore { target })
    }
}

/// The canonical `&'static` name of a window-family verb.
pub fn window_verb(verb: &str) -> Option<&'static str> {
    WINDOW_VERBS.iter().copied().find(|known| *known == verb)
}

/// A parsed `comp.region.select` identity or a typed `comp.region.cancel`:
/// the compositor process instance the selection was sent to (the
/// `comp.info` instance), the caller's owner capability and the positive
/// capture generation. All three are required together; the identity is
/// echoed on terminal replies and is what `comp.region.cancel` matches
/// exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionIdentity {
    pub instance: String,
    pub owner: String,
    pub generation: u64,
}

impl SelectionIdentity {
    /// The strict `{instance, owner, generation}` wire object.
    pub fn wire_value(&self) -> Value {
        json!({
            "instance": self.instance,
            "owner": self.owner,
            "generation": self.generation,
        })
    }
}

/// A random UUID v4, lowercase hyphenated: the owner capability shape
/// (`xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx`). The owner is a targeting
/// capability, not an authentication claim.
pub fn valid_owner_capability(owner: &str) -> bool {
    let bytes = owner.as_bytes();
    if bytes.len() != 36 || [8, 13, 18, 23].iter().any(|at| bytes[*at] != b'-') {
        return false;
    }
    [0, 1, 2, 3, 5, 6, 7, 9, 10, 11, 12, 16, 17, 21, 22]
        .into_iter()
        .chain(24..36)
        .all(|at| bytes[at].is_ascii_hexdigit())
        && bytes[14] == b'4'
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
}

/// The strict `{instance, owner, generation}` identity object. Unknown
/// fields are refused by name, so a typo can never act on the wrong
/// generation. `None` (or `null`) with `required` false is the legacy
/// identity-less selection.
fn selection_arg(
    value: Option<&Value>,
    required: bool,
) -> Result<Option<SelectionIdentity>, ControlReply> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return if required {
            Err(invalid_argument(
                "selection",
                "object",
                "{instance, owner, generation}",
            ))
        } else {
            Ok(None)
        };
    };
    const SELECTION: &[&str] = &["instance", "owner", "generation"];
    let object = match value {
        Value::Object(object) => object,
        _ => {
            return Err(invalid_argument(
                "selection",
                "object",
                "{instance, owner, generation}",
            ));
        }
    };
    if let Some(field) = object
        .keys()
        .find(|field| !SELECTION.contains(&field.as_str()))
    {
        return Err(ControlReply::InvalidArgs {
            field: format!("selection.{field}"),
            allowed: SELECTION,
        });
    }
    let instance = match object.get("instance") {
        Some(Value::String(instance))
            if !instance.is_empty() && instance.is_ascii() && instance.len() <= 128 =>
        {
            instance.clone()
        }
        _ => {
            return Err(invalid_argument(
                "selection.instance",
                "string",
                "the comp.info instance (1..=128 ascii bytes)",
            ));
        }
    };
    let owner = match object.get("owner") {
        Some(Value::String(owner)) if valid_owner_capability(owner) => owner.clone(),
        _ => {
            return Err(invalid_argument(
                "selection.owner",
                "string",
                "a random UUID v4",
            ));
        }
    };
    let generation = match object.get("generation") {
        Some(value) => value
            .as_u64()
            .filter(|generation| *generation >= 1)
            .ok_or_else(|| {
                invalid_argument("selection.generation", "unsigned integer", "1..=u64::MAX")
            })?,
        None => {
            return Err(invalid_argument(
                "selection.generation",
                "unsigned integer",
                "required (positive)",
            ));
        }
    };
    Ok(Some(SelectionIdentity {
        instance,
        owner,
        generation,
    }))
}

/// The defaults for `comp.window.wait` and `comp.window.close {force}`.
pub const WINDOW_WAIT_DEFAULT: Duration = Duration::from_secs(10);
pub const CLOSE_FORCE_DEFAULT: Duration = Duration::from_secs(3);

/// The required `{id, generation}` window target: both unsigned integers,
/// `null` reads as absent, and the refusal names the missing field.
pub fn required_target(
    object: &serde_json::Map<String, Value>,
) -> Result<(u64, u64), ControlReply> {
    let id = window_arg(object, "id")?
        .ok_or_else(|| invalid_argument("id", "unsigned integer", "required"))?;
    let generation = window_arg(object, "generation")?.ok_or_else(|| {
        invalid_argument(
            "generation",
            "unsigned integer",
            "required (read windows.s<id>.generation)",
        )
    })?;
    Ok((id, generation))
}

fn bool_arg(
    object: &serde_json::Map<String, Value>,
    name: &'static str,
    default: bool,
) -> Result<bool, ControlReply> {
    match present(object, name) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(invalid_argument(name, "bool", "true|false")),
    }
}

/// `index` of the workspace verbs: an unsigned integer `>= 1`, `"next"` or
/// `"prev"`. `0` is refused here as `invalid_value` (the contract: outside
/// `1..count`, 0 included); an index above the count is refused by the
/// compositor, which knows the count.
fn workspace_index_arg(
    object: &serde_json::Map<String, Value>,
) -> Result<WorkspaceIndex, ControlReply> {
    match present(object, "index") {
        Some(Value::String(step)) if step == "next" => Ok(WorkspaceIndex::Next),
        Some(Value::String(step)) if step == "prev" => Ok(WorkspaceIndex::Prev),
        Some(value) if value.as_u64().is_some_and(|index| index >= 1) => value
            .as_u64()
            .and_then(|index| u32::try_from(index).ok())
            .map(WorkspaceIndex::Absolute)
            .ok_or_else(|| workspace_index_refusal("1..=workspaces.count|next|prev")),
        Some(_) => Err(workspace_index_refusal("1..=workspaces.count|next|prev")),
        None => Err(workspace_index_refusal(
            "required: 1..=workspaces.count|next|prev",
        )),
    }
}

fn workspace_index_refusal(range: &'static str) -> ControlReply {
    invalid_argument("index", "unsigned integer or next|prev", range)
}

/// `output` of a placement or a workspace switch: a non-empty `outputs`
/// key or output name.
fn output_arg(object: &serde_json::Map<String, Value>) -> Result<Option<String>, ControlReply> {
    match present(object, "output") {
        None => Ok(None),
        Some(Value::String(output)) if !output.is_empty() => Ok(Some(output.clone())),
        Some(_) => Err(invalid_argument(
            "output",
            "string",
            "outputs.<key> key or output name",
        )),
    }
}

fn size_arg(
    object: &serde_json::Map<String, Value>,
    name: &'static str,
) -> Result<Option<i32>, ControlReply> {
    match present(object, name) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|size| (1..=32_767).contains(size))
            .map(|size| Some(size as i32))
            .ok_or_else(|| invalid_argument(name, "integer", "1..=32767")),
    }
}

fn string_arg(
    object: &serde_json::Map<String, Value>,
    name: &'static str,
) -> Result<Option<String>, ControlReply> {
    match present(object, name) {
        None => Ok(None),
        Some(Value::String(value)) if value.len() <= 4096 => Ok(Some(value.clone())),
        Some(_) => Err(invalid_argument(name, "string", "at most 4096 bytes")),
    }
}

fn timeout_arg(
    object: &serde_json::Map<String, Value>,
    default: Duration,
) -> Result<Duration, ControlReply> {
    match present(object, "timeout_ms") {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|ms| (1..=LONG_VERB_MAX.as_millis() as u64).contains(ms))
            .map(Duration::from_millis)
            .ok_or_else(|| invalid_argument("timeout_ms", "unsigned integer", "1..=60000")),
    }
}

/// Every `comp.window.*` verb. Minimise and restore keep their own parser.
pub fn parse_window_verb(verb: &str, args: &Value) -> Result<WindowVerb, ControlReply> {
    let empty = serde_json::Map::new();
    match verb {
        "comp.hardware.snapshot" => {
            args_object(args, &empty, &[])?;
            Ok(WindowVerb::Op(WindowOp::HardwareSnapshot))
        }
        "comp.hardware.wait" => {
            let object = args_object(args, &empty, &["instance", "after", "until", "timeout_ms"])?;
            let instance = string_arg(object, "instance")?
                .filter(|value| !value.is_empty() && value.len() <= 128)
                .ok_or_else(|| {
                    invalid_argument(
                        "instance",
                        "string",
                        "required nonempty compositor incarnation",
                    )
                })?;
            let after = present(object, "after")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    invalid_argument("after", "unsigned integer", "required native sequence")
                })?;
            let until = match present(object, "until").and_then(Value::as_str) {
                Some("keyboard") => HardwareUntil::Keyboard,
                Some("pointer") => HardwareUntil::Pointer,
                Some("paused") => HardwareUntil::Paused,
                Some("active") => HardwareUntil::Active,
                _ => {
                    return Err(invalid_argument(
                        "until",
                        "string",
                        "keyboard|pointer|paused|active",
                    ));
                }
            };
            Ok(WindowVerb::Long(LongOp::HardwareWait(HardwareWaitSpec {
                instance,
                after,
                until,
                timeout: timeout_arg(object, Duration::from_secs(20))?,
            })))
        }
        "comp.world.list" | "comp.world.create" => {
            args_object(args, &empty, &[])?;
            Ok(WindowVerb::Op(if verb == "comp.world.list" {
                WindowOp::WorldList
            } else {
                WindowOp::WorldCreate
            }))
        }
        "comp.world.activate" => {
            let object = args_object(args, &empty, &["id"])?;
            let raw = present(object, "id")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_argument("id", "UUID string", "required"))?;
            let id = uuid::Uuid::parse_str(raw).map_err(|_| {
                invalid_argument("id", "UUID string", "canonical hyphenated UUID required")
            })?;
            if raw != id.to_string() {
                return Err(invalid_argument(
                    "id",
                    "UUID string",
                    "canonical hyphenated UUID required",
                ));
            }
            Ok(WindowVerb::Op(WindowOp::WorldActivate { id }))
        }
        "comp.window.tile" | "comp.window.untile" => {
            let allowed: &'static [&'static str] = if verb == "comp.window.tile" {
                &["id", "generation", "output"]
            } else {
                &["id", "generation"]
            };
            let object = args_object(args, &empty, allowed)?;
            let (id, generation) = required_target(object)?;
            Ok(WindowVerb::Op(WindowOp::Tile {
                id,
                generation,
                enabled: verb == "comp.window.tile",
                output: output_arg(object)?,
            }))
        }
        "comp.window.maximize"
        | "comp.window.unmaximize"
        | "comp.window.fullscreen"
        | "comp.window.unfullscreen" => {
            let allowed: &'static [&'static str] = if verb == "comp.window.fullscreen" {
                &["id", "generation", "output"]
            } else {
                &["id", "generation"]
            };
            let object = args_object(args, &empty, allowed)?;
            let (id, generation) = required_target(object)?;
            Ok(WindowVerb::Op(WindowOp::State {
                id,
                generation,
                state: if verb.ends_with("maximize") {
                    WindowState::Maximized
                } else {
                    WindowState::Fullscreen
                },
                enabled: matches!(verb, "comp.window.maximize" | "comp.window.fullscreen"),
                output: output_arg(object)?,
            }))
        }
        "comp.window.minimize" | "comp.window.restore" => {
            parse_window_op(verb, args).map(WindowVerb::Op)
        }
        "comp.window.stats" | "comp.window.stats.reset" => {
            parse_stats_op(verb, args).map(WindowVerb::Op)
        }
        "comp.window.focus" => {
            const ALLOWED: &[&str] = &["id", "generation", "raise"];
            let object = args_object(args, &empty, ALLOWED)?;
            let (id, generation) = required_target(object)?;
            Ok(WindowVerb::Op(WindowOp::Focus {
                id,
                generation,
                raise: bool_arg(object, "raise", true)?,
            }))
        }
        "comp.window.raise" => {
            const ALLOWED: &[&str] = &["id", "generation"];
            let object = args_object(args, &empty, ALLOWED)?;
            let (id, generation) = required_target(object)?;
            Ok(WindowVerb::Op(WindowOp::Raise { id, generation }))
        }
        "comp.window.close" => {
            const ALLOWED: &[&str] = &["id", "generation", "force", "timeout_ms"];
            let object = args_object(args, &empty, ALLOWED)?;
            let (id, generation) = required_target(object)?;
            if bool_arg(object, "force", false)? {
                Ok(WindowVerb::Long(LongOp::ForceClose {
                    id,
                    generation,
                    timeout: timeout_arg(object, CLOSE_FORCE_DEFAULT)?,
                }))
            } else if present(object, "timeout_ms").is_some() {
                Err(invalid_argument(
                    "timeout_ms",
                    "absent",
                    "timeout_ms applies with force:true only",
                ))
            } else {
                Ok(WindowVerb::Op(WindowOp::Close { id, generation }))
            }
        }
        "comp.window.place" => {
            const ALLOWED: &[&str] = &["id", "generation", "output", "x", "y", "width", "height"];
            let object = args_object(args, &empty, ALLOWED)?;
            let (id, generation) = required_target(object)?;
            let output = output_arg(object)?;
            let spec = PlaceSpec {
                id,
                generation,
                x: finite_arg(object, "x")?,
                y: finite_arg(object, "y")?,
                width: size_arg(object, "width")?,
                height: size_arg(object, "height")?,
                output,
            };
            if spec.output.is_none()
                && spec.x.is_none()
                && spec.y.is_none()
                && spec.width.is_none()
                && spec.height.is_none()
            {
                return Err(invalid_argument(
                    "x",
                    "finite number",
                    "place needs at least one of output, x, y, width, height",
                ));
            }
            Ok(WindowVerb::Op(WindowOp::Place(spec)))
        }
        "comp.workspace.switch" => {
            const ALLOWED: &[&str] = &["index", "output", "wrap"];
            let object = args_object(args, &empty, ALLOWED)?;
            Ok(WindowVerb::Op(WindowOp::SwitchWorkspace {
                index: workspace_index_arg(object)?,
                output: output_arg(object)?,
                wrap: bool_arg(object, "wrap", true)?,
            }))
        }
        "comp.window.send_to_workspace" => {
            const ALLOWED: &[&str] = &["id", "generation", "index", "follow"];
            let object = args_object(args, &empty, ALLOWED)?;
            let (id, generation) = required_target(object)?;
            Ok(WindowVerb::Op(WindowOp::SendToWorkspace {
                id,
                generation,
                index: workspace_index_arg(object)?,
                follow: bool_arg(object, "follow", false)?,
            }))
        }
        "comp.window.wait" => {
            const ALLOWED: &[&str] = &["match", "until", "width", "height", "timeout_ms"];
            const MATCH: &[&str] = &["id", "generation", "app_id", "title", "title_contains"];
            let object = args_object(args, &empty, ALLOWED)?;
            let filters = match present(object, "match") {
                Some(Value::Object(filters)) => filters,
                _ => {
                    return Err(invalid_argument(
                        "match",
                        "object",
                        "{id?, generation?, app_id?, title?, title_contains?}",
                    ));
                }
            };
            if let Some(field) = filters
                .keys()
                .find(|field| !MATCH.contains(&field.as_str()))
            {
                return Err(ControlReply::InvalidArgs {
                    field: format!("match.{field}"),
                    allowed: MATCH,
                });
            }
            let window = WindowMatch {
                id: window_arg(filters, "id")?,
                generation: window_arg(filters, "generation")?,
                app_id: string_arg(filters, "app_id")?,
                title: string_arg(filters, "title")?,
                title_contains: string_arg(filters, "title_contains")?,
            };
            if window.id.is_some()
                && (window.app_id.is_some()
                    || window.title.is_some()
                    || window.title_contains.is_some())
            {
                return Err(invalid_argument(
                    "match",
                    "object",
                    "match by id or by app_id/title/title_contains, not both",
                ));
            }
            if window.generation.is_some() && window.id.is_none() {
                return Err(invalid_argument(
                    "match.id",
                    "unsigned integer",
                    "required with generation",
                ));
            }
            if window == WindowMatch::default() {
                return Err(invalid_argument(
                    "match",
                    "object",
                    "at least one of id, app_id, title, title_contains",
                ));
            }
            let width = size_arg(object, "width")?;
            let height = size_arg(object, "height")?;
            let until = match present(object, "until").and_then(Value::as_str) {
                Some("tiled") => WaitUntil::Tiled,
                Some("untiled") => WaitUntil::Untiled,
                Some("mapped") => WaitUntil::Mapped,
                Some("visible") => WaitUntil::Visible,
                Some("presented") => WaitUntil::Presented,
                Some("size") => match (width, height) {
                    (Some(width), Some(height)) => WaitUntil::Size { width, height },
                    _ => {
                        return Err(invalid_argument(
                            "width",
                            "integer",
                            "until:size needs width and height",
                        ));
                    }
                },
                Some("focused") => WaitUntil::Focused,
                Some("maximized") => WaitUntil::Maximized,
                Some("unmaximized") => WaitUntil::Unmaximized,
                Some("fullscreen") => WaitUntil::Fullscreen,
                Some("unfullscreen") => WaitUntil::Unfullscreen,
                Some("unmapped") => WaitUntil::Unmapped,
                Some("gone") => WaitUntil::Gone,
                _ => {
                    return Err(invalid_argument(
                        "until",
                        "string",
                        "mapped|visible|presented|size|focused|maximized|unmaximized|fullscreen|unfullscreen|tiled|untiled|unmapped|gone",
                    ));
                }
            };
            if !matches!(until, WaitUntil::Size { .. }) && (width.is_some() || height.is_some()) {
                return Err(invalid_argument(
                    "width",
                    "absent",
                    "width and height apply to until:size only",
                ));
            }
            Ok(WindowVerb::Long(LongOp::Wait(WaitSpec {
                window,
                until,
                timeout: timeout_arg(object, WINDOW_WAIT_DEFAULT)?,
            })))
        }
        _ => Err(invalid_argument("verb", "window verb", "comp.window.*")),
    }
}

/// The canonical `&'static` name of a single-step input verb.
pub fn input_verb(verb: &str) -> Option<&'static str> {
    INPUT_VERBS.iter().copied().find(|known| *known == verb)
}

/// evdev `BTN_LEFT` / `BTN_RIGHT` / `BTN_MIDDLE`.
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;
/// evdev `KEY_MAX`: every key and button code is at most this.
const EVDEV_CODE_MAX: u64 = 0x2ff;

fn args_object<'a>(
    args: &'a Value,
    empty: &'a serde_json::Map<String, Value>,
    allowed: &'static [&'static str],
) -> Result<&'a serde_json::Map<String, Value>, ControlReply> {
    let object = match args {
        Value::Null => empty,
        Value::Object(object) => object,
        _ => return Err(invalid_argument("args", "JSON object", "verb arguments")),
    };
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(ControlReply::InvalidArgs {
            field: field.clone(),
            allowed,
        });
    }
    Ok(object)
}

fn present<'a>(object: &'a serde_json::Map<String, Value>, name: &str) -> Option<&'a Value> {
    object.get(name).filter(|value| !value.is_null())
}

fn finite_arg(
    object: &serde_json::Map<String, Value>,
    name: &'static str,
) -> Result<Option<f64>, ControlReply> {
    match present(object, name) {
        None => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|value| value.is_finite() && value.abs() <= 1.0e6)
            .map(Some)
            .ok_or_else(|| invalid_argument(name, "finite number", "-1e6..=1e6")),
    }
}

fn required_finite(
    object: &serde_json::Map<String, Value>,
    name: &'static str,
) -> Result<f64, ControlReply> {
    finite_arg(object, name)?.ok_or_else(|| invalid_argument(name, "finite number", "required"))
}

fn press_action(
    object: &serde_json::Map<String, Value>,
    both: &'static str,
) -> Result<PressAction, ControlReply> {
    match present(object, "action") {
        None => Ok(PressAction::Both),
        Some(Value::String(action)) if action == "press" => Ok(PressAction::Press),
        Some(Value::String(action)) if action == "release" => Ok(PressAction::Release),
        Some(Value::String(action)) if action == both => Ok(PressAction::Both),
        Some(_) => Err(invalid_argument(
            "action",
            "string",
            if both == "click" {
                "press|release|click"
            } else {
                "press|release|tap"
            },
        )),
    }
}

fn evdev_code(value: &Value, name: &'static str, minimum: u64) -> Result<u32, ControlReply> {
    value
        .as_u64()
        .filter(|code| (minimum..=EVDEV_CODE_MAX).contains(code))
        .map(|code| code as u32)
        .ok_or_else(|| invalid_argument(name, "evdev code", "an evdev code up to 0x2ff"))
}

fn key_spec(value: &Value, name: &'static str) -> Result<KeySpec, ControlReply> {
    match value {
        Value::String(key) if !key.is_empty() && key.len() <= 64 => Ok(KeySpec::Name(key.clone())),
        Value::Number(_) => evdev_code(value, name, 1).map(KeySpec::Evdev),
        _ => Err(invalid_argument(
            name,
            "keysym name or evdev code",
            "XKB keysym name (\"Return\", \"a\") or 1..=0x2ff",
        )),
    }
}

fn modifier_spec(value: &Value) -> Result<KeySpec, ControlReply> {
    let name = match value.as_str() {
        Some("shift") => "Shift_L",
        Some("ctrl" | "control") => "Control_L",
        Some("alt") => "Alt_L",
        Some("super" | "logo") => "Super_L",
        Some("altgr") => "ISO_Level3_Shift",
        _ => {
            return Err(invalid_argument(
                "modifiers",
                "list of modifier names",
                "shift|ctrl|alt|super|altgr",
            ));
        }
    };
    Ok(KeySpec::Name(name.into()))
}

/// Parse one `comp.input.*` verb's arguments. Shared by the direct verbs
/// and `comp.input.sequence` steps, so a step is exactly the verb.
pub fn parse_input_op(verb: &str, args: &Value) -> Result<InputOp, ControlReply> {
    let explicit_seat = parse_input_seat(args.get("seat"))?;
    let seat = explicit_seat.unwrap_or(DEFAULT_INPUT_SEAT);
    let mut args = args.clone();
    if let Some(object) = args.as_object_mut() {
        object.remove("seat");
    }
    let op = parse_seated_input_op(verb, &args, seat)?;
    // Bare cleanup always releases both seats' injected holds, regardless of
    // the delivery default. An explicit (including inherited) seat scopes it.
    if verb == "comp.input.release_all" && explicit_seat.is_none() {
        return Ok(op);
    }
    // Keep the internal human operation shape stable for existing call sites.
    Ok(
        if seat == SeatKind::Human && !(verb == "comp.input.release_all" && explicit_seat.is_some())
        {
            op
        } else {
            InputOp::OnSeat {
                seat,
                op: Box::new(op),
            }
        },
    )
}

// Human-semantic callers must select human explicitly; the hub gates migrate
// with this release. Bare release_all remains both-seat cleanup above.
pub const DEFAULT_INPUT_SEAT: SeatKind = SeatKind::Agent;

fn parse_input_seat(value: Option<&Value>) -> Result<Option<SeatKind>, ControlReply> {
    match value {
        None => Ok(None),
        Some(Value::String(value)) if value == "human" => Ok(Some(SeatKind::Human)),
        Some(Value::String(value)) if value == "agent" => Ok(Some(SeatKind::Agent)),
        _ => Err(invalid_argument("seat", "string", "agent|human")),
    }
}

fn parse_seated_input_op(
    verb: &str,
    args: &Value,
    seat: SeatKind,
) -> Result<InputOp, ControlReply> {
    let op = parse_input_payload(verb, args)?;
    if !matches!(verb, "comp.input.key" | "comp.input.pointer.button") {
        return Ok(op);
    }
    let empty = serde_json::Map::new();
    let object = args.as_object().unwrap_or(&empty);
    let Some(window) = present(object, "window") else {
        if present(object, "raise").is_some() {
            return Err(invalid_argument("raise", "absent", "requires window"));
        }
        return Ok(op);
    };
    const WINDOW: &[&str] = &["id", "generation"];
    let Value::Object(window) = window else {
        return Err(invalid_argument("window", "object", "{id, generation}"));
    };
    if let Some(field) = window
        .keys()
        .find(|field| !WINDOW.contains(&field.as_str()))
    {
        return Err(ControlReply::InvalidArgs {
            field: format!("window.{field}"),
            allowed: WINDOW,
        });
    }
    let id = window_arg(window, "id")?
        .ok_or_else(|| invalid_argument("window.id", "unsigned integer", "required"))?;
    let generation = window_arg(window, "generation")?
        .ok_or_else(|| invalid_argument("window.generation", "unsigned integer", "required"))?;
    let raise = bool_arg(object, "raise", seat == SeatKind::Human)?;
    if seat == SeatKind::Agent && raise {
        return Err(ControlReply::refused(
            "invalid_argument",
            json!({
                "field":"raise", "seat":"agent", "message":"agent input never raises a window",
            }),
        ));
    }
    Ok(InputOp::Targeted {
        id,
        generation,
        raise,
        op: Box::new(op),
    })
}

fn parse_input_payload(verb: &str, args: &Value) -> Result<InputOp, ControlReply> {
    let empty = serde_json::Map::new();
    match verb {
        "comp.input.pointer.move" => {
            const ALLOWED: &[&str] = &[
                "output",
                "x",
                "y",
                "dx",
                "dy",
                "window",
                "require_hit",
                "corners",
            ];
            let object = args_object(args, &empty, ALLOWED)?;
            let corners = bool_arg(object, "corners", true)?;
            let moved = |target| Ok(InputOp::PointerMove { target, corners });
            let require_hit = match present(object, "require_hit") {
                None => None,
                Some(Value::Bool(value)) => Some(*value),
                Some(_) => return Err(invalid_argument("require_hit", "bool", "true|false")),
            };
            let relative = present(object, "dx").is_some() || present(object, "dy").is_some();
            if let Some(window) = present(object, "window") {
                const WINDOW: &[&str] = &["id", "generation"];
                let window = match window {
                    Value::Object(window) => window,
                    _ => return Err(invalid_argument("window", "object", "{id, generation}")),
                };
                if let Some(field) = window
                    .keys()
                    .find(|field| !WINDOW.contains(&field.as_str()))
                {
                    return Err(ControlReply::InvalidArgs {
                        field: format!("window.{field}"),
                        allowed: WINDOW,
                    });
                }
                if relative || present(object, "output").is_some() {
                    return Err(invalid_argument(
                        "window",
                        "exclusive form",
                        "{window, x, y} takes no output, dx or dy",
                    ));
                }
                let id = window_arg(window, "id")?
                    .ok_or_else(|| invalid_argument("window.id", "unsigned integer", "required"))?;
                let generation = window_arg(window, "generation")?.ok_or_else(|| {
                    invalid_argument(
                        "window.generation",
                        "unsigned integer",
                        "required (read windows.s<id>.generation)",
                    )
                })?;
                return moved(PointerMoveTarget::Window {
                    id,
                    generation,
                    x: required_finite(object, "x")?,
                    y: required_finite(object, "y")?,
                    require_hit: require_hit.unwrap_or(false),
                });
            }
            if require_hit.is_some() {
                return Err(invalid_argument(
                    "require_hit",
                    "absent",
                    "require_hit applies to the {window, x, y} form only",
                ));
            }
            if relative {
                if present(object, "x").is_some()
                    || present(object, "y").is_some()
                    || present(object, "output").is_some()
                {
                    return Err(invalid_argument(
                        "dx",
                        "exclusive form",
                        "{dx, dy} takes no output, x or y",
                    ));
                }
                return moved(PointerMoveTarget::Relative {
                    dx: finite_arg(object, "dx")?.unwrap_or(0.0),
                    dy: finite_arg(object, "dy")?.unwrap_or(0.0),
                });
            }
            let output = match present(object, "output") {
                None => None,
                Some(Value::String(output)) if !output.is_empty() => Some(output.clone()),
                Some(_) => {
                    return Err(invalid_argument(
                        "output",
                        "string",
                        "outputs.<key> key or output name",
                    ));
                }
            };
            moved(PointerMoveTarget::Output {
                output,
                x: required_finite(object, "x")?,
                y: required_finite(object, "y")?,
            })
        }
        "comp.input.pointer.button" => {
            const ALLOWED: &[&str] = &["button", "action", "window", "raise"];
            let object = args_object(args, &empty, ALLOWED)?;
            let button = match present(object, "button") {
                None => BTN_LEFT,
                Some(Value::String(name)) if name == "left" => BTN_LEFT,
                Some(Value::String(name)) if name == "right" => BTN_RIGHT,
                Some(Value::String(name)) if name == "middle" => BTN_MIDDLE,
                Some(value @ Value::Number(_)) => evdev_code(value, "button", 0x100)?,
                Some(_) => {
                    return Err(invalid_argument(
                        "button",
                        "button name or evdev code",
                        "left|right|middle|0x100..=0x2ff",
                    ));
                }
            };
            Ok(InputOp::PointerButton {
                button,
                action: press_action(object, "click")?,
            })
        }
        "comp.input.pointer.scroll" => {
            const ALLOWED: &[&str] = &["dx", "dy", "source", "v120"];
            let object = args_object(args, &empty, ALLOWED)?;
            let dx = finite_arg(object, "dx")?;
            let dy = finite_arg(object, "dy")?;
            if dx.is_none() && dy.is_none() {
                return Err(invalid_argument(
                    "dy",
                    "finite number",
                    "dx or dy is required",
                ));
            }
            let source = match present(object, "source") {
                None => ScrollSource::Wheel,
                Some(Value::String(source)) if source == "wheel" => ScrollSource::Wheel,
                Some(Value::String(source)) if source == "finger" => ScrollSource::Finger,
                Some(Value::String(source)) if source == "continuous" => ScrollSource::Continuous,
                Some(_) => {
                    return Err(invalid_argument(
                        "source",
                        "string",
                        "wheel|finger|continuous",
                    ));
                }
            };
            let detent = |name: &'static str,
                          value: Option<&Value>,
                          amount: Option<f64>|
             -> Result<Option<i32>, ControlReply> {
                match value {
                    Some(value) => {
                        if amount.is_none() {
                            return Err(invalid_argument(
                                name,
                                "absent",
                                "a detent count needs the matching axis",
                            ));
                        }
                        value
                            .as_i64()
                            .and_then(|value| i32::try_from(value).ok())
                            .filter(|value| value.unsigned_abs() <= 120 * 1000)
                            .map(Some)
                            .ok_or_else(|| invalid_argument(name, "integer", "-120000..=120000"))
                    }
                    // A wheel reports detents: 15 logical units to one
                    // detent (120) is libinput's convention. Other sources
                    // have none, and an absent count stays absent.
                    None if source == ScrollSource::Wheel => {
                        Ok(amount.map(|amount| (amount * 8.0).round() as i32))
                    }
                    None => Ok(None),
                }
            };
            let v120 = match present(object, "v120") {
                None => (detent("v120.dx", None, dx)?, detent("v120.dy", None, dy)?),
                Some(Value::Object(v120)) => {
                    const V120: &[&str] = &["dx", "dy"];
                    if let Some(field) = v120.keys().find(|field| !V120.contains(&field.as_str())) {
                        return Err(ControlReply::InvalidArgs {
                            field: format!("v120.{field}"),
                            allowed: V120,
                        });
                    }
                    if source != ScrollSource::Wheel {
                        return Err(invalid_argument(
                            "v120",
                            "absent",
                            "detent counts belong to source wheel",
                        ));
                    }
                    (
                        detent("v120.dx", present(v120, "dx"), dx)?,
                        detent("v120.dy", present(v120, "dy"), dy)?,
                    )
                }
                Some(_) => return Err(invalid_argument("v120", "object", "{dx?, dy?}")),
            };
            Ok(InputOp::PointerScroll {
                dx,
                dy,
                source,
                v120,
            })
        }
        "comp.input.key" => {
            const ALLOWED: &[&str] = &["key", "action", "modifiers", "text", "window", "raise"];
            let object = args_object(args, &empty, ALLOWED)?;
            if let Some(text) = present(object, "text") {
                if ["key", "action", "modifiers"]
                    .iter()
                    .any(|name| present(object, name).is_some())
                {
                    return Err(invalid_argument(
                        "text",
                        "exclusive form",
                        "{text} takes no key, action or modifiers",
                    ));
                }
                return match text {
                    Value::String(text)
                        if !text.is_empty() && text.chars().count() <= TEXT_MAX_CHARS =>
                    {
                        Ok(InputOp::Text(text.clone()))
                    }
                    _ => Err(invalid_argument("text", "string", "1..=256 characters")),
                };
            }
            let key = present(object, "key")
                .ok_or_else(|| invalid_argument("key", "keysym name or evdev code", "required"))
                .and_then(|key| key_spec(key, "key"))?;
            let modifiers = match present(object, "modifiers") {
                None => Vec::new(),
                Some(Value::Array(modifiers)) if modifiers.len() <= 5 => modifiers
                    .iter()
                    .map(modifier_spec)
                    .collect::<Result<Vec<_>, _>>()?,
                Some(_) => {
                    return Err(invalid_argument(
                        "modifiers",
                        "list of modifier names",
                        "shift|ctrl|alt|super|altgr",
                    ));
                }
            };
            Ok(InputOp::Key {
                key,
                action: press_action(object, "tap")?,
                modifiers,
            })
        }
        "comp.input.release_all" => {
            args_object(args, &empty, &[])?;
            Ok(InputOp::ReleaseAll)
        }
        _ => Err(invalid_argument(
            "verb",
            "input verb",
            "comp.input.pointer.move|pointer.button|pointer.scroll|key|release_all",
        )),
    }
}

fn delay_arg(value: Option<&Value>, name: &'static str) -> Result<Option<Duration>, ControlReply> {
    match value {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|ms| *ms <= LONG_VERB_MAX.as_millis() as u64)
            .map(|ms| Some(Duration::from_millis(ms)))
            .ok_or_else(|| invalid_argument(name, "unsigned integer", "0..=60000")),
    }
}

/// `comp.region.select {output?, timeout_ms?, selection?}`: the selection
/// deadline is 1..=55000 ms (default 30 s), leaving reply margin inside the
/// 60 s cap. `selection` is the optional strict caller identity; a legacy
/// select without one keeps its legacy reply shape.
pub fn parse_region_select(args: &Value) -> Result<LongOp, ControlReply> {
    let empty = serde_json::Map::new();
    let object = args_object(args, &empty, &["output", "timeout_ms", "selection"])?;
    let output = match object.get("output") {
        None => None,
        Some(Value::String(name)) if !name.is_empty() => Some(name.clone()),
        _ => {
            return Err(invalid_argument(
                "output",
                "non-empty string",
                "output name",
            ));
        }
    };
    let timeout = match object.get("timeout_ms") {
        None => 30_000,
        Some(value) => value
            .as_u64()
            .filter(|ms| (1..=55_000).contains(ms))
            .ok_or_else(|| {
                invalid_argument("timeout_ms", "integer 1..55000", "selection deadline")
            })?,
    };
    let selection = selection_arg(object.get("selection"), false)?;
    Ok(LongOp::RegionSelect {
        output,
        timeout: Duration::from_millis(timeout),
        selection,
    })
}

/// `comp.region.cancel {selection}`: a short owner operation admitted
/// independently of the long pool. It cancels only the exact active
/// selection and otherwise retires the identity, so a reordered mesh
/// delivery can never resurrect a retired generation.
pub fn parse_region_cancel(args: &Value) -> Result<SelectionIdentity, ControlReply> {
    let empty = serde_json::Map::new();
    let object = args_object(args, &empty, &["selection"])?;
    selection_arg(object.get("selection"), true)?
        .ok_or_else(|| invalid_argument("selection", "object", "{instance, owner, generation}"))
}

/// `comp.input.sequence {steps:[{verb, args?, delay_ms?}], interval_ms?}`.
/// `delay_ms` (default `interval_ms`, default 0) runs before its step; the
/// delays together are capped at 60 s.
pub fn parse_sequence(args: &Value) -> Result<LongOp, ControlReply> {
    let empty = serde_json::Map::new();
    const ALLOWED: &[&str] = &["steps", "interval_ms", "seat"];
    let object = args_object(args, &empty, ALLOWED)?;
    let seat = parse_input_seat(object.get("seat"))?;
    let interval = delay_arg(present(object, "interval_ms"), "interval_ms")?.unwrap_or_default();
    let steps = match present(object, "steps") {
        Some(Value::Array(steps)) if !steps.is_empty() && steps.len() <= SEQUENCE_MAX_STEPS => {
            steps
        }
        _ => return Err(invalid_argument("steps", "list", "1..=256 steps")),
    };
    let mut parsed = Vec::with_capacity(steps.len());
    let mut total = Duration::ZERO;
    let mut events = 0_usize;
    for (index, step) in steps.iter().enumerate() {
        const STEP: &[&str] = &["verb", "args", "delay_ms"];
        let step = match step {
            Value::Object(step) => step,
            _ => {
                return Err(invalid_argument(
                    "steps",
                    "list of objects",
                    "{verb, args?, delay_ms?}",
                ));
            }
        };
        if let Some(field) = step.keys().find(|field| !STEP.contains(&field.as_str())) {
            return Err(ControlReply::InvalidArgs {
                field: format!("steps[{index}].{field}"),
                allowed: STEP,
            });
        }
        let verb = present(step, "verb")
            .and_then(Value::as_str)
            .and_then(input_verb)
            .ok_or_else(|| {
                invalid_argument(
                    "steps.verb",
                    "input verb",
                    "comp.input.pointer.move|pointer.button|pointer.scroll|key|release_all",
                )
            })?;
        let mut args = step.get("args").cloned().unwrap_or(Value::Null);
        if let Some(seat) = seat {
            if args.is_null() {
                args = json!({});
            }
            if let Some(object) = args.as_object_mut() {
                object.entry("seat").or_insert_with(|| json!(seat.name()));
            }
        }
        let op = parse_input_op(verb, &args).map_err(|reply| match reply {
            ControlReply::Validation(SetValidationError::InvalidValue {
                path,
                expected,
                range,
            }) => ControlReply::Validation(SetValidationError::InvalidValue {
                path: format!("steps[{index}].args.{path}"),
                expected,
                range,
            }),
            ControlReply::InvalidArgs { field, allowed } => ControlReply::InvalidArgs {
                field: format!("steps[{index}].args.{field}"),
                allowed,
            },
            other => other,
        })?;
        events += op.event_bound();
        if events > MAX_EVENTS_PER_VERB {
            return Err(invalid_argument(
                "steps",
                "injected events",
                "at most 4096 injected events per verb (MAX_EVENTS_PER_VERB)",
            ));
        }
        let delay = delay_arg(present(step, "delay_ms"), "steps.delay_ms")?.unwrap_or(interval);
        total += delay;
        if total > LONG_VERB_MAX {
            return Err(invalid_argument(
                "steps",
                "total delay",
                "the delays together are at most 60000 ms",
            ));
        }
        parsed.push(SequenceStep { verb, op, delay });
    }
    Ok(match seat {
        Some(seat) => LongOp::SeatedSequence {
            seat,
            steps: parsed,
        },
        None => LongOp::Sequence(parsed),
    })
}

/// A content source id: `[a-z0-9_-]{1,64}`.
pub fn valid_content_source_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
}

/// `comp.window.stats {id, generation | source, registration?, samples?}`
/// and `comp.window.stats.reset {id, generation | source, registration? |
/// nothing}`. `{id, generation}` and `{source}` are mutually exclusive.
pub fn parse_stats_op(verb: &str, args: &Value) -> Result<WindowOp, ControlReply> {
    const STATS_ARGS: &[&str] = &["id", "generation", "source", "registration", "samples"];
    const RESET_ARGS: &[&str] = &["id", "generation", "source", "registration"];
    let reset = verb == "comp.window.stats.reset";
    let empty = serde_json::Map::new();
    let object = match args {
        Value::Null => &empty,
        Value::Object(object) => object,
        _ => {
            return Err(invalid_argument(
                "args",
                "JSON object",
                "{id, generation} or {source}",
            ));
        }
    };
    let allowed = if reset { RESET_ARGS } else { STATS_ARGS };
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(ControlReply::InvalidArgs {
            field: field.clone(),
            allowed,
        });
    }
    let id = window_arg(object, "id")?;
    let generation = window_arg(object, "generation")?;
    let registration = window_arg(object, "registration")?;
    let source = match object.get("source") {
        None | Some(Value::Null) => None,
        Some(Value::String(source)) if valid_content_source_id(source) => Some(source.clone()),
        Some(_) => {
            return Err(invalid_argument("source", "string", "[a-z0-9_-]{1,64}"));
        }
    };
    let window = match (id, generation) {
        (Some(id), Some(generation)) => Some((id, generation)),
        (None, None) => None,
        (Some(_), None) => {
            return Err(invalid_argument(
                "generation",
                "unsigned integer",
                "required with id (read windows.s<id>.generation)",
            ));
        }
        (None, Some(_)) => {
            return Err(invalid_argument(
                "id",
                "unsigned integer",
                "required with generation",
            ));
        }
    };
    let target = match (window, source) {
        (Some(_), Some(_)) => {
            return Err(invalid_argument(
                "source",
                "absent when id is given",
                "{id, generation} and {source} are mutually exclusive",
            ));
        }
        (Some((id, generation)), None) => {
            if registration.is_some() {
                return Err(invalid_argument(
                    "registration",
                    "absent when id is given",
                    "only with source (read sources.<id>.registration)",
                ));
            }
            Some(StatsTarget::Window { id, generation })
        }
        (None, Some(id)) => Some(StatsTarget::Source { id, registration }),
        (None, None) => {
            if registration.is_some() {
                return Err(invalid_argument(
                    "source",
                    "string",
                    "required with registration",
                ));
            }
            None
        }
    };
    if reset {
        return Ok(WindowOp::StatsReset { target });
    }
    let Some(target) = target else {
        return Err(invalid_argument(
            "id",
            "unsigned integer",
            "required (with generation), or give source",
        ));
    };
    let samples = match window_arg(object, "samples")? {
        None => STATS_RING,
        Some(samples) if samples <= STATS_RING as u64 => samples as usize,
        Some(_) => {
            return Err(invalid_argument("samples", "unsigned integer", "0..=512"));
        }
    };
    Ok(WindowOp::Stats { target, samples })
}

/// The tree path a read verb can reach; `None` for the whole tree.
///
/// A `list` prefix and a scope relate to paths the same way: at segment
/// boundaries (`PropPath::starts_with`, `ReadScopes::wants`). A mid-segment
/// prefix such as `"windows.s"` therefore lists nothing and scopes nothing,
/// consistently, so the raw prefix is the scope.
pub fn read_scope(verb: &str, args: &Value) -> Option<String> {
    // Explicit per verb: a verb added to `needs_snapshot` later must say
    // what its scope is, rather than inherit "path" and be mis-scoped by a
    // same-named argument that means something else.
    let key = match verb {
        "comp.info" => return Some("info".to_string()),
        "comp.props.list" => "prefix",
        "comp.props.get" | "comp.props.describe" => "path",
        _ => return None,
    };
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

/// The `comp.props.set` refusal for a body that is not `{path, value}`.
pub fn invalid_set_shape(path: Option<&str>) -> (u8, Arc<str>) {
    (
        10,
        Arc::from(
            json!({
                "error": "invalid_value",
                "path": path,
                "expected": "JSON property value",
                "range": "descriptor",
            })
            .to_string(),
        ),
    )
}

/// Whether a non-empty request body failed to parse as JSON. Computed once
/// per command; each verb family refuses it its own way.
pub fn body_is_malformed(body: &str) -> bool {
    !body.is_empty() && serde_json::from_str::<Value>(body).is_err()
}

/// The `comp.ping` success body.
pub const PING_BODY: &str = "{\"pong\":true}";

/// One routed `comp.*` request, ready for admission. What happens next
/// (permits, ingress, ordering fences) is the transport's.
#[derive(Clone, Debug)]
pub enum Request {
    /// Answered at once with [`PING_BODY`], even with a malformed body.
    Ping,
    /// `comp.props.watch`.
    Watch,
    /// `comp.pointer.watch`.
    PointerWatch,
    /// `comp.props.set`, already through the ingress gate.
    Set {
        path: String,
        value: Value,
        generation: Option<u64>,
    },
    /// A window-family verb answered in one pass.
    Window(WindowOp),
    /// A verb whose reply waits: `window.wait`, `window.close {force}`,
    /// `region.select`, `input.sequence`. Admit with
    /// [`LongOp::admission_timeout`].
    Long(LongOp),
    /// `comp.region.cancel {selection}`: a short owner operation, admitted
    /// independently of the long pool.
    RegionCancel(SelectionIdentity),
    /// A single-step input verb.
    Input(InputOp),
    /// `comp.panel.hold` / `comp.panel.mode`. The transport stamps
    /// `sender` from the broker's `from` before admitting it.
    Panel(PanelRequest),
    /// A snapshot read (`comp.info`, `comp.props.get|list|describe`,
    /// `comp.windows.list`) and the subtree that scopes its snapshot.
    Read {
        verb: &'static str,
        scope: Option<String>,
    },
}

/// Route one command through the verb families, in order, with each
/// family's own refusal for a malformed body. `Err` is the immediate reply (before the transport adds
/// `error_code` with [`crate::reply::with_error_code`]). Verbs stay literal
/// `comp.*` whatever the service is registered as, so
/// `comp-nested.panel.hold` is `unknown_verb`.
pub fn classify(command: &str, args: &Value, malformed: bool) -> Result<Request, (u8, Arc<str>)> {
    if command == "comp.capture.frame" {
        if malformed {
            return Err(
                invalid_argument("args", "JSON object", "{output?, path, format?}").into_wire(),
            );
        }
        return crate::capture::parse(args)
            .map(|spec| Request::Long(LongOp::CaptureFrame(spec)))
            .map_err(ControlReply::into_wire);
    }
    if command == "comp.ping" {
        return Ok(Request::Ping);
    }
    if command == "comp.props.watch" || command == "comp.pointer.watch" {
        if malformed {
            return Err(error("unknown_path"));
        }
        return Ok(if command == "comp.pointer.watch" {
            Request::PointerWatch
        } else {
            Request::Watch
        });
    }
    if command == "comp.props.set" {
        let (path, value, generation) = if malformed {
            return Err(invalid_set_shape(None));
        } else {
            parse_set(args)?
        };
        if let Err(error) = validate_set_request(&path, &value) {
            return Err(ControlReply::Validation(error).into_wire());
        }
        return Ok(Request::Set {
            path,
            value,
            generation,
        });
    }
    if let Some(verb) = window_verb(command) {
        let parsed = if malformed {
            Err(invalid_argument("args", "JSON object", "{id, generation}"))
        } else {
            parse_window_verb(verb, args)
        };
        return match parsed {
            Ok(WindowVerb::Op(op)) => Ok(Request::Window(op)),
            Ok(WindowVerb::Long(op)) => Ok(Request::Long(op)),
            Err(reply) => Err(reply.into_wire()),
        };
    }
    if command == "comp.region.select" {
        let parsed = if malformed {
            Err(invalid_argument(
                "args",
                "JSON object",
                "{output?, timeout_ms?, selection?}",
            ))
        } else {
            parse_region_select(args)
        };
        return parsed.map(Request::Long).map_err(ControlReply::into_wire);
    }
    if command == "comp.region.cancel" {
        let parsed = if malformed {
            Err(invalid_argument("args", "JSON object", "{selection}"))
        } else {
            parse_region_cancel(args)
        };
        return parsed
            .map(Request::RegionCancel)
            .map_err(ControlReply::into_wire);
    }
    if command == "comp.input.sequence" {
        let parsed = if malformed {
            Err(invalid_argument(
                "args",
                "JSON object",
                "{steps, interval_ms?}",
            ))
        } else {
            parse_sequence(args)
        };
        return parsed.map(Request::Long).map_err(ControlReply::into_wire);
    }
    if let Some(verb) = input_verb(command) {
        let parsed = if malformed {
            Err(invalid_argument("args", "JSON object", "verb arguments"))
        } else {
            parse_input_op(verb, args)
        };
        return parsed.map(Request::Input).map_err(ControlReply::into_wire);
    }
    if matches!(command, "comp.panel.hold" | "comp.panel.mode") {
        let parsed = if malformed {
            Err(invalid_argument(
                "args",
                "JSON object",
                "{output, edge, surface, ...}",
            ))
        } else {
            PanelRequest::parse(command, args)
        };
        return parsed.map(Request::Panel).map_err(ControlReply::into_wire);
    }
    let Some(verb) = READ_VERBS.iter().copied().find(|known| *known == command) else {
        return Err(error("unknown_verb"));
    };
    if malformed {
        return Err(error("unknown_path"));
    }
    Ok(Request::Read {
        verb,
        scope: read_scope(verb, args),
    })
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
