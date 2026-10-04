// The hot-corner detector and corner press ownership. `Corner` and
// `CornerConfig` live in comp-model (they are on the wire); the press map
// is a value the engine owns, fed the facts the engine reads (lock, engaged
// corner, modifiers); the topics come back as `CornerTopic`s to number with
// the shared event sequence.

//! Allocation-free compositor-side hot-corner detection, in output-local
//! logical coordinates. Defaults: a 10 px hotspot, a 200 ms dwell and a
//! 1500 px/s speed cap; an outward push engages early.

use std::collections::BTreeMap;
use std::time::Duration;

use comp_model::observation::ObservationRecord;
use comp_model::request::{BTN_LEFT, BTN_RIGHT};

pub use comp_model::observation::{Corner, CornerConfig};

const MOTION_EPSILON_PX: f64 = 0.001;
const MIN_STATIONARY_CONFIRM_MS: u64 = 1;

/// The fields that shape detection. Drawing-only leaves are excluded so
/// toggling them never ends an engagement.
fn detection(config: CornerConfig) -> (bool, f64, u64, f64) {
    (
        config.enabled,
        config.deadzone_px,
        config.dwell_ms,
        config.velocity_max_px_s,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CornerEvent {
    Entered { corner: Corner, dwell_ms: u64 },
    Left { corner: Corner, dwell_ms: u64 },
}

pub type CornerEvents = [Option<CornerEvent>; 2];

#[derive(Clone, Copy, Debug)]
struct Candidate {
    corner: Corner,
    entered_at_ms: u64,
    velocity_eligible_since_ms: Option<u64>,
    last_speed_px_s: Option<f64>,
}

#[derive(Clone, Copy, Debug)]
struct Engagement {
    corner: Corner,
    dwell_ms: u64,
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    at_ms: u64,
    position: (f64, f64),
}

#[derive(Clone, Debug)]
pub struct CornerDetector {
    config: CornerConfig,
    size: (f64, f64),
    candidate: Option<Candidate>,
    engaged: Option<Engagement>,
    last_sample: Option<Sample>,
}

impl CornerDetector {
    pub fn new(config: CornerConfig, size: (f64, f64)) -> Self {
        Self {
            config,
            size,
            candidate: None,
            engaged: None,
            last_sample: None,
        }
    }

    pub fn engaged_corner(&self) -> Option<Corner> {
        self.engaged.map(|e| e.corner)
    }

    /// The hotspot the pointer is in, dwelled or not.
    pub fn contact_corner(&self) -> Option<Corner> {
        self.engaged
            .map(|engaged| engaged.corner)
            .or(self.candidate.map(|candidate| candidate.corner))
    }

    pub fn engaged_dwell_ms(&self) -> Option<u64> {
        self.engaged.map(|e| e.dwell_ms)
    }

    /// When the pending dwell completes: the one-shot timer the engine arms
    /// while a candidate exists (none otherwise, so nothing polls).
    pub fn next_deadline_ms(&self) -> Option<u64> {
        let candidate = self.candidate?;
        candidate.velocity_eligible_since_ms.map_or_else(
            || {
                self.last_sample.map(|sample| {
                    sample.at_ms.saturating_add(if self.config.dwell_ms == 0 {
                        MIN_STATIONARY_CONFIRM_MS
                    } else {
                        self.config.dwell_ms
                    })
                })
            },
            |since| Some(since.saturating_add(self.config.dwell_ms)),
        )
    }

    pub fn candidate_position(&self) -> Option<(f64, f64)> {
        self.candidate
            .and_then(|_| self.last_sample.map(|sample| sample.position))
    }

    pub fn sample(
        &mut self,
        at_ms: u64,
        position: (f64, f64),
        attempted_motion: (f64, f64),
    ) -> CornerEvents {
        if !self.config.enabled
            || !self.config.valid()
            || !valid_size(self.size)
            || !point_inside(position, self.size)
            || self
                .last_sample
                .is_some_and(|previous| at_ms < previous.at_ms)
        {
            return self.reset();
        }

        let sample = Sample { at_ms, position };
        let speed = self.instantaneous_speed(sample);
        let corner = corner_at(position, self.size, self.config.deadzone_px);
        let mut events = [None, None];
        let mut next = 0;

        if let Some(engaged) = self.engaged {
            if corner == Some(engaged.corner) {
                self.last_sample = Some(sample);
                return events;
            }
            self.engaged = None;
            events[next] = Some(CornerEvent::Left {
                corner: engaged.corner,
                dwell_ms: engaged.dwell_ms,
            });
            next += 1;
        }

        let Some(corner) = corner else {
            self.candidate = None;
            self.last_sample = Some(sample);
            return events;
        };

        let continuing = self
            .candidate
            .is_some_and(|candidate| candidate.corner == corner);
        let mut became_velocity_eligible = false;
        if !continuing {
            self.candidate = Some(Candidate {
                corner,
                entered_at_ms: at_ms,
                velocity_eligible_since_ms: speed
                    .is_some_and(|value| value <= self.config.velocity_max_px_s)
                    .then_some(at_ms),
                last_speed_px_s: speed,
            });
        } else if let Some(speed) = speed
            && let Some(candidate) = &mut self.candidate
        {
            candidate.last_speed_px_s = Some(speed);
            if speed <= self.config.velocity_max_px_s {
                if candidate.velocity_eligible_since_ms.is_none() {
                    became_velocity_eligible = true;
                    candidate.velocity_eligible_since_ms =
                        Some(self.last_sample.map_or(at_ms, |previous| previous.at_ms));
                }
            } else {
                candidate.velocity_eligible_since_ms = None;
            }
        }

        let candidate = self.candidate.expect("corner candidate exists");
        let dwell_ms = at_ms.saturating_sub(candidate.entered_at_ms);
        let velocity_dwell_complete = candidate
            .velocity_eligible_since_ms
            .is_some_and(|since| at_ms.saturating_sub(since) >= self.config.dwell_ms);
        let outward_push = continuing
            && !became_velocity_eligible
            && candidate
                .last_speed_px_s
                .is_some_and(|value| value <= self.config.velocity_max_px_s)
            && pushes_outward(corner, attempted_motion);
        if velocity_dwell_complete || outward_push {
            self.candidate = None;
            self.engaged = Some(Engagement { corner, dwell_ms });
            events[next] = Some(CornerEvent::Entered { corner, dwell_ms });
        }
        self.last_sample = Some(sample);
        events
    }

    pub fn reset(&mut self) -> CornerEvents {
        self.candidate = None;
        self.last_sample = None;
        [
            self.engaged.take().map(|engaged| CornerEvent::Left {
                corner: engaged.corner,
                dwell_ms: engaged.dwell_ms,
            }),
            None,
        ]
    }

    pub fn reconfigure(&mut self, config: CornerConfig, size: (f64, f64)) -> CornerEvents {
        let changed = detection(self.config) != detection(config) || self.size != size;
        self.config = config;
        self.size = size;
        if changed || !config.enabled || !config.valid() || !valid_size(size) {
            self.reset()
        } else {
            [None, None]
        }
    }

    fn instantaneous_speed(&self, sample: Sample) -> Option<f64> {
        let previous = self.last_sample?;
        let elapsed_ms = sample.at_ms.checked_sub(previous.at_ms)?;
        if elapsed_ms == 0 {
            return None;
        }
        let movement = (sample.position.0 - previous.position.0)
            .hypot(sample.position.1 - previous.position.1);
        Some(if movement > MOTION_EPSILON_PX {
            movement / Duration::from_millis(elapsed_ms).as_secs_f64()
        } else {
            0.0
        })
    }
}

fn valid_size(size: (f64, f64)) -> bool {
    size.0.is_finite() && size.1.is_finite() && size.0 > 0.0 && size.1 > 0.0
}

fn point_inside(point: (f64, f64), size: (f64, f64)) -> bool {
    point.0.is_finite()
        && point.1.is_finite()
        && point.0 >= 0.0
        && point.1 >= 0.0
        && point.0 < size.0
        && point.1 < size.1
}

fn corner_at(point: (f64, f64), size: (f64, f64), deadzone: f64) -> Option<Corner> {
    let left = point.0 <= deadzone;
    let right = size.0 - point.0 <= deadzone;
    let top = point.1 <= deadzone;
    let bottom = size.1 - point.1 <= deadzone;
    let candidates = [
        (left && top, Corner::TopLeft, point.0.hypot(point.1)),
        (
            left && bottom,
            Corner::BottomLeft,
            point.0.hypot(size.1 - point.1),
        ),
        (
            right && bottom,
            Corner::BottomRight,
            (size.0 - point.0).hypot(size.1 - point.1),
        ),
        (
            right && top,
            Corner::TopRight,
            (size.0 - point.0).hypot(point.1),
        ),
    ];
    candidates
        .into_iter()
        .filter(|(inside, _, _)| *inside)
        .min_by(|left, right| left.2.total_cmp(&right.2))
        .map(|(_, corner, _)| corner)
}

/// Whether the attempted motion pushes out of the screen at `corner`: the
/// early engage a cursor pinned at the edge gets.
pub fn pushes_outward(corner: Corner, motion: (f64, f64)) -> bool {
    match corner {
        Corner::TopLeft => motion.0 < 0.0 || motion.1 < 0.0,
        Corner::TopRight => motion.0 > 0.0 || motion.1 < 0.0,
        Corner::BottomLeft => motion.0 < 0.0 || motion.1 > 0.0,
        Corner::BottomRight => motion.0 > 0.0 || motion.1 > 0.0,
    }
}

/// One corner topic, before the shared event sequence numbers it.
#[derive(Clone, Debug, PartialEq)]
pub enum CornerTopic {
    Entered { output: String, corner: Corner, dwell_ms: u64 },
    Left { output: String, corner: Corner, dwell_ms: u64 },
    /// The legacy click (an unmodified left click only).
    Clicked { output: String, corner: Corner, dwell_ms: u64 },
    ClickedV2 {
        output: String,
        corner: Corner,
        dwell_ms: u64,
        button: &'static str,
        kind: &'static str,
        modifiers: Vec<&'static str>,
    },
}

impl CornerTopic {
    /// The detector's events on output `output` (its `o_<slug>` key).
    pub fn from_events(output: &str, events: CornerEvents) -> Vec<Self> {
        events
            .into_iter()
            .flatten()
            .map(|event| match event {
                CornerEvent::Entered { corner, dwell_ms } => Self::Entered {
                    output: output.to_string(),
                    corner,
                    dwell_ms,
                },
                CornerEvent::Left { corner, dwell_ms } => Self::Left {
                    output: output.to_string(),
                    corner,
                    dwell_ms,
                },
            })
            .collect()
    }

    pub fn into_record(self, event_seq: u64) -> ObservationRecord {
        match self {
            Self::Entered { output, corner, dwell_ms } => ObservationRecord::CornerEntered {
                output,
                corner,
                dwell_ms,
                event_seq,
            },
            Self::Left { output, corner, dwell_ms } => ObservationRecord::CornerLeft {
                output,
                corner,
                dwell_ms,
                event_seq,
            },
            Self::Clicked { output, corner, dwell_ms } => ObservationRecord::CornerClicked {
                output,
                corner,
                dwell_ms,
                event_seq,
            },
            Self::ClickedV2 {
                output,
                corner,
                dwell_ms,
                button,
                kind,
                modifiers,
            } => ObservationRecord::CornerClickedV2 {
                output,
                corner,
                dwell_ms,
                button,
                kind,
                modifiers,
                event_seq,
            },
        }
    }
}

/// A press the compositor took on an engaged corner, owed a click at its release.
#[derive(Clone, Debug, PartialEq)]
pub struct CornerPress {
    pub output: String,
    pub corner: Corner,
    pub dwell_ms: u64,
    pub position: (f64, f64),
    /// Held at the press (`shift`, `ctrl`, `alt`, `super`); the release
    /// may differ.
    pub modifiers: Vec<&'static str>,
}

/// What the engine knows at a press.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PressFacts {
    pub session_lock: bool,
    /// The engaged corner, its output key and its dwell.
    pub engaged: Option<(String, Corner, u64)>,
    pub position: (f64, f64),
    pub modifiers: Vec<&'static str>,
}

/// Buttons the compositor owns because their press landed on an engaged corner. A
/// press it owns never reaches a client, and nor does its release, so the
/// ownership survives every reset; only the click itself can be cancelled.
#[derive(Clone, Debug, Default)]
pub struct CornerPresses {
    presses: BTreeMap<u32, Option<CornerPress>>,
}

impl CornerPresses {
    /// A button went down. Returns whether the compositor consumes it (it never
    /// reaches a client). A second press of an owned button stays owned.
    pub fn consume_press(&mut self, button: u32, facts: PressFacts) -> bool {
        if self.presses.contains_key(&button) {
            return true;
        }
        if facts.session_lock {
            return false;
        }
        let Some((output, corner, dwell_ms)) = facts.engaged else {
            return false;
        };
        let action = (button == BTN_LEFT || button == BTN_RIGHT).then_some(CornerPress {
            output,
            corner,
            dwell_ms,
            position: facts.position,
            modifiers: facts.modifiers,
        });
        self.presses.insert(button, action);
        true
    }

    /// A button came up. `None`: not owned (deliver it). `Some(topics)`:
    /// consumed, and these are the click topics it earned (empty when the
    /// click was cancelled or the button was not left/right). The engine
    /// re-samples the corner at the pointer before acting on it.
    pub fn consume_release(&mut self, button: u32) -> Option<Vec<CornerTopic>> {
        let press = self.presses.remove(&button)?;
        Some(press.map_or_else(Vec::new, |press| click_topics(press, button)))
    }

    /// Cancel every owed click (the corner left, the lock came up), keeping
    /// the ownership.
    pub fn cancel_all(&mut self) {
        for action in self.presses.values_mut() {
            *action = None;
        }
    }

    /// The pointer moved: a click whose pointer strayed past the hotspot
    /// size from its press is cancelled.
    pub fn pointer_moved(&mut self, position: (f64, f64), deadzone: f64) {
        for action in self.presses.values_mut() {
            let Some(press) = action else { continue };
            if (position.0 - press.position.0).hypot(position.1 - press.position.1) > deadzone {
                *action = None;
            }
        }
    }

    pub fn owns(&self, button: u32) -> bool {
        self.presses.contains_key(&button)
    }

    /// Whether `button` is owned and its click is still owed (the engine
    /// re-samples the corner before such a release).
    pub fn owes_click(&self, button: u32) -> bool {
        self.presses.get(&button).is_some_and(Option::is_some)
    }

    /// The owned buttons, ascending.
    pub fn owned(&self) -> impl Iterator<Item = u32> + '_ {
        self.presses.keys().copied()
    }

    pub fn is_empty(&self) -> bool {
        self.presses.is_empty()
    }
}

/// The topics of a recognised click: an unmodified left click also emits
/// the legacy `corner.clicked`, then every click its `corner.clicked.v2`.
fn click_topics(press: CornerPress, button: u32) -> Vec<CornerTopic> {
    let left = button == BTN_LEFT;
    let mut topics = Vec::new();
    if left && press.modifiers.is_empty() {
        topics.push(CornerTopic::Clicked {
            output: press.output.clone(),
            corner: press.corner,
            dwell_ms: press.dwell_ms,
        });
    }
    topics.push(CornerTopic::ClickedV2 {
        output: press.output,
        corner: press.corner,
        dwell_ms: press.dwell_ms,
        button: if left { "left" } else { "right" },
        kind: "brief",
        modifiers: press.modifiers,
    });
    topics
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (f64, f64) = (1_000.0, 800.0);

    fn detector() -> CornerDetector {
        CornerDetector::new(CornerConfig::default(), SIZE)
    }

    fn entered(events: CornerEvents) -> Option<(Corner, u64)> {
        events.into_iter().flatten().find_map(|event| match event {
            CornerEvent::Entered { corner, dwell_ms } => Some((corner, dwell_ms)),
            CornerEvent::Left { .. } => None,
        })
    }

    #[test]
    fn names_all_four_corners() {
        assert_eq!(Corner::TopLeft.name(), "tl");
        assert_eq!(Corner::TopRight.name(), "tr");
        assert_eq!(Corner::BottomLeft.name(), "bl");
        assert_eq!(Corner::BottomRight.name(), "br");
    }

    #[test]
    fn slow_stationary_dwell_enters_once_then_leaves() {
        let mut detector = detector();
        assert_eq!(detector.sample(0, (5.0, 5.0), (0.0, 0.0)), [None, None]);
        assert_eq!(detector.next_deadline_ms(), Some(200));
        assert_eq!(
            entered(detector.sample(200, (5.0, 5.0), (0.0, 0.0))),
            Some((Corner::TopLeft, 200))
        );
        assert_eq!(detector.sample(250, (5.0, 5.0), (-4.0, 0.0)), [None, None]);
        assert_eq!(
            detector.sample(300, (50.0, 50.0), (0.0, 0.0))[0],
            Some(CornerEvent::Left {
                corner: Corner::TopLeft,
                dwell_ms: 200
            })
        );
    }

    #[test]
    fn each_corner_enters() {
        for (position, corner) in [
            ((1.0, 1.0), Corner::TopLeft),
            ((999.0, 1.0), Corner::TopRight),
            ((1.0, 799.0), Corner::BottomLeft),
            ((999.0, 799.0), Corner::BottomRight),
        ] {
            let mut detector = detector();
            detector.sample(0, position, (0.0, 0.0));
            assert_eq!(
                entered(detector.sample(200, position, (0.0, 0.0))),
                Some((corner, 200))
            );
        }
    }

    #[test]
    fn fast_transit_waits_for_stationary_dwell() {
        let mut detector = detector();
        detector.sample(0, (100.0, 100.0), (0.0, 0.0));
        assert_eq!(entered(detector.sample(10, (5.0, 5.0), (0.0, 0.0))), None);
        assert_eq!(
            entered(detector.sample(210, (5.0, 5.0), (0.0, 0.0))),
            Some((Corner::TopLeft, 200))
        );
    }

    #[test]
    fn outward_push_enters_early_but_inward_push_does_not() {
        let mut outward = detector();
        outward.sample(0, (5.0, 5.0), (0.0, 0.0));
        outward.sample(20, (5.0, 5.0), (0.0, 0.0));
        assert_eq!(
            entered(outward.sample(40, (5.0, 5.0), (-3.0, 0.0))),
            Some((Corner::TopLeft, 40))
        );

        let mut inward = detector();
        inward.sample(0, (5.0, 5.0), (0.0, 0.0));
        assert_eq!(entered(inward.sample(20, (5.0, 5.0), (3.0, 3.0))), None);
    }

    #[test]
    fn reset_and_geometry_or_config_change_emit_left() {
        let mut detector = detector();
        detector.sample(0, (5.0, 5.0), (0.0, 0.0));
        detector.sample(200, (5.0, 5.0), (0.0, 0.0));
        assert!(matches!(
            detector.reset()[0],
            Some(CornerEvent::Left { .. })
        ));

        detector.sample(300, (5.0, 5.0), (0.0, 0.0));
        detector.sample(500, (5.0, 5.0), (0.0, 0.0));
        assert!(matches!(
            detector.reconfigure(CornerConfig::default(), (900.0, 800.0))[0],
            Some(CornerEvent::Left { .. })
        ));
    }

    #[test]
    fn disabled_invalid_and_non_monotonic_samples_suppress_engagement() {
        let mut config = CornerConfig {
            enabled: false,
            ..CornerConfig::default()
        };
        let mut disabled = CornerDetector::new(config, SIZE);
        assert_eq!(disabled.sample(0, (5.0, 5.0), (-1.0, 0.0)), [None, None]);
        config.enabled = true;
        config.deadzone_px = f64::NAN;
        disabled.reconfigure(config, SIZE);
        assert_eq!(disabled.sample(1, (5.0, 5.0), (-1.0, 0.0)), [None, None]);

        let mut detector = detector();
        detector.sample(10, (5.0, 5.0), (0.0, 0.0));
        assert_eq!(detector.sample(9, (5.0, 5.0), (-1.0, 0.0)), [None, None]);
    }

    #[test]
    fn default_hotspot_is_ten_logical_units_and_edges_are_inclusive() {
        let config = CornerConfig::default();
        assert_eq!(config.deadzone_px, 10.0);
        assert!(config.valid());
        assert!(config.affordance, "hover reveal is on unless disabled");
        assert!(
            !config.discovery,
            "the shell opts in to the first-run flash"
        );
        assert_eq!(corner_at((10.0, 10.0), SIZE, 10.0), Some(Corner::TopLeft));
        assert_eq!(corner_at((10.5, 1.0), SIZE, 10.0), None);
        assert_eq!(corner_at((11.0, 11.0), SIZE, 10.0), None);
    }

    #[test]
    fn affordance_and_discovery_toggles_do_not_end_an_engagement() {
        let mut detector = detector();
        detector.sample(0, (5.0, 5.0), (0.0, 0.0));
        detector.sample(200, (5.0, 5.0), (0.0, 0.0));
        assert_eq!(detector.engaged_corner(), Some(Corner::TopLeft));
        let config = CornerConfig {
            affordance: false,
            discovery: true,
            ..CornerConfig::default()
        };
        assert_eq!(detector.reconfigure(config, SIZE), [None, None]);
        assert_eq!(detector.engaged_corner(), Some(Corner::TopLeft));
    }

    #[test]
    fn zero_dwell_still_requires_a_follow_up_sample() {
        let config = CornerConfig {
            dwell_ms: 0,
            ..CornerConfig::default()
        };
        let mut detector = CornerDetector::new(config, SIZE);
        assert_eq!(entered(detector.sample(0, (5.0, 5.0), (0.0, 0.0))), None);
        assert_eq!(detector.next_deadline_ms(), Some(1));
        assert_eq!(
            entered(detector.sample(1, (5.0, 5.0), (0.0, 0.0))),
            Some((Corner::TopLeft, 1))
        );
    }

    // ---- The defaults and press ownership. ----

    #[test]
    fn defaults_are_comps() {
        let config = CornerConfig::default();
        assert_eq!(
            (config.deadzone_px, config.dwell_ms, config.velocity_max_px_s, config.enabled),
            (10.0, 200, 1_500.0, true)
        );
        assert!(pushes_outward(Corner::BottomRight, (1.0, 0.0)));
        assert!(!pushes_outward(Corner::BottomRight, (-1.0, -1.0)));
    }

    fn engaged_press(position: (f64, f64), modifiers: Vec<&'static str>) -> PressFacts {
        PressFacts {
            session_lock: false,
            engaged: Some(("o_dp_1".into(), Corner::BottomRight, 217)),
            position,
            modifiers,
        }
    }

    #[test]
    fn an_engaged_press_is_owned_until_its_release_and_earns_its_clicks() {
        let mut presses = CornerPresses::default();
        assert!(!presses.consume_press(BTN_LEFT, PressFacts::default()), "nothing engaged");
        assert!(presses.consume_release(BTN_LEFT).is_none(), "not ours");
        assert!(presses.consume_press(BTN_LEFT, engaged_press((995.0, 795.0), Vec::new())));
        assert!(presses.consume_press(BTN_LEFT, PressFacts::default()), "a repeat stays owned");
        let topics = presses.consume_release(BTN_LEFT).expect("ours");
        assert_eq!(
            topics,
            [
                CornerTopic::Clicked {
                    output: "o_dp_1".into(),
                    corner: Corner::BottomRight,
                    dwell_ms: 217,
                },
                CornerTopic::ClickedV2 {
                    output: "o_dp_1".into(),
                    corner: Corner::BottomRight,
                    dwell_ms: 217,
                    button: "left",
                    kind: "brief",
                    modifiers: Vec::new(),
                },
            ]
        );
        assert!(presses.is_empty());
        // A modified or right click is v2 only.
        presses.consume_press(BTN_RIGHT, engaged_press((995.0, 795.0), vec!["shift"]));
        let topics = presses.consume_release(BTN_RIGHT).unwrap();
        assert!(matches!(
            topics.as_slice(),
            [CornerTopic::ClickedV2 { button: "right", modifiers, .. }] if modifiers == &["shift"]
        ));
        // Another button is owned but earns nothing.
        presses.consume_press(0x112, engaged_press((995.0, 795.0), Vec::new()));
        assert_eq!(presses.consume_release(0x112), Some(Vec::new()));
        // The lock refuses a new press.
        let locked = PressFacts {
            session_lock: true,
            ..engaged_press((995.0, 795.0), Vec::new())
        };
        assert!(!presses.consume_press(BTN_LEFT, locked));
    }

    #[test]
    fn a_strayed_or_cancelled_click_keeps_ownership_but_earns_nothing() {
        let mut presses = CornerPresses::default();
        presses.consume_press(BTN_LEFT, engaged_press((995.0, 795.0), Vec::new()));
        presses.pointer_moved((990.0, 790.0), 10.0);
        assert_eq!(presses.consume_release(BTN_LEFT).map(|topics| topics.len()), Some(2), "within the hotspot");
        presses.consume_press(BTN_LEFT, engaged_press((995.0, 795.0), Vec::new()));
        presses.pointer_moved((900.0, 700.0), 10.0);
        assert!(presses.owns(BTN_LEFT));
        assert_eq!(presses.consume_release(BTN_LEFT), Some(Vec::new()));
        presses.consume_press(BTN_LEFT, engaged_press((995.0, 795.0), Vec::new()));
        presses.cancel_all();
        assert_eq!(presses.consume_release(BTN_LEFT), Some(Vec::new()));
    }

    #[test]
    fn detector_events_become_numbered_topics() {
        let mut detector = detector();
        detector.sample(0, (5.0, 5.0), (0.0, 0.0));
        let topics = CornerTopic::from_events("o_dp_1", detector.sample(200, (5.0, 5.0), (0.0, 0.0)));
        let record = topics.into_iter().next().unwrap().into_record(9);
        assert_eq!(record.topic_suffix(), "corner.entered");
        assert_eq!(
            record.wire().body,
            r#"{"corner":"tl","dwell_ms":200,"event_seq":9,"output":"o_dp_1"}"#
        );
    }
}
