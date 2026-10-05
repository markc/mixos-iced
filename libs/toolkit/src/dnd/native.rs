// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native drag and drop. Tokens identify a compositor-authorised
//! pointer press or offer. Backends validate origin, seat and liveness.
use std::sync::Arc;

/// A pointer press that can authorise one drag from its originating window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Gesture(pub u64);

/// An offer addressed to a particular window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Offer(pub u64);

/// A negotiated operation. Move completes only after the target finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Copy,
    Move,
}

/// Source or target capabilities. At least one action must be enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actions {
    pub copy: bool,
    pub move_: bool,
}
impl Actions {
    pub const COPY: Self = Self {
        copy: true,
        move_: false,
    };
    pub const MOVE: Self = Self {
        copy: false,
        move_: true,
    };
    pub const BOTH: Self = Self {
        copy: true,
        move_: true,
    };
    pub fn contains(self, action: Action) -> bool {
        match action {
            Action::Copy => self.copy,
            Action::Move => self.move_,
        }
    }
}

/// A bounded MIME payload supplied before starting the native drag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub mime: String,
    pub bytes: Arc<[u8]>,
    pub actions: Actions,
}
impl Source {
    pub const MAX_BYTES: usize = 16 * 1024 * 1024;
    pub fn valid(&self) -> bool {
        !self.mime.is_empty()
            && self.mime.len() <= 255
            && !self.mime.chars().any(|c| c.is_control())
            && self.bytes.len() <= Self::MAX_BYTES
            && (self.actions.copy || self.actions.move_)
    }
}

/// Queued operations are confirmed or rejected through window events.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Sent immediately before the corresponding left-button press.
    Gesture(Gesture),
    Started(Gesture),
    Rejected(Gesture),
    Enter {
        offer: Offer,
        position: iced_core::Point,
        mimes: Vec<String>,
    },
    Motion {
        offer: Offer,
        position: iced_core::Point,
    },
    Action {
        offer: Offer,
        action: Option<Action>,
    },
    Leave(Offer),
    Drop(Offer),
    /// Bytes are delivered once, at EOF. The caller must decode/apply them
    /// before calling finish_drag_offer; a physical drop alone is insufficient.
    Data {
        offer: Offer,
        mime: String,
        bytes: Arc<[u8]>,
        action: Action,
    },
    Failed(Offer),
    Finished {
        gesture: Gesture,
        action: Action,
    },
    Cancelled(Gesture),
}

/// Backend is unavailable, or the queued request itself is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    Invalid,
}

/// A request queued onto the owning native window backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Start(Gesture, Source),
    Accept(Offer, Option<String>, Actions, Action),
    Receive(Offer, String),
    Finish(Offer, bool),
    Cancel(Gesture),
}

/// Caller-selected MIME encoding. Decoding must reject invalid input rather
/// than applying a partial payload. The session also bounds encoded bytes.
pub trait Codec<P> {
    fn mime(&self) -> &str;
    fn encode(&self, payload: &P) -> Result<Vec<u8>, String>;
    fn decode(&self, bytes: &[u8]) -> Result<P, String>;
}

/// A standard UTF-8 text codec, interoperable with native desktop clients.
pub struct Text;
impl Codec<String> for Text {
    fn mime(&self) -> &str {
        "text/plain;charset=utf-8"
    }
    fn encode(&self, payload: &String) -> Result<Vec<u8>, String> {
        Ok(payload.as_bytes().to_vec())
    }
    fn decode(&self, bytes: &[u8]) -> Result<String, String> {
        String::from_utf8(bytes.to_vec()).map_err(|error| error.to_string())
    }
}

/// Backend work and caller-owned state changes are explicit. Applying a
/// delivery requires an acknowledgement; source Move is confirmed only by
/// Finished after the native target acknowledged success.
#[derive(Debug)]
pub enum Effect<P, W> {
    Request {
        window: W,
        request: Request,
    },
    Delivery {
        window: W,
        offer: Offer,
        payload: P,
        action: Action,
    },
    Finished {
        window: W,
        payload: P,
        action: Action,
    },
    Cancelled {
        window: W,
    },
    Failed {
        window: W,
        offer: Offer,
    },
}
struct Outgoing<P, W> {
    window: W,
    gesture: Gesture,
    payload: P,
    started: bool,
    actions: Actions,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Entered,
    Receiving,
    Delivered,
}
struct Incoming {
    supported: bool,
    accepted: bool,
    action: Option<Action>,
    phase: Phase,
}

/// Typed native DnD state independent of the window backend. Feed events from
/// that backend and execute returned requests on their owning window. A
/// transport without native support must report Unsupported, never simulate
/// cross-window transfers through shared in-process payloads.
pub struct Session<P, W> {
    codec: Box<dyn Codec<P>>,
    presses: std::collections::HashMap<W, Gesture>,
    outgoing: Option<Outgoing<P, W>>,
    incoming: std::collections::HashMap<(W, Offer), Incoming>,
}
impl<P, W: Clone + Eq + std::hash::Hash> Session<P, W> {
    pub fn new(codec: impl Codec<P> + 'static) -> Self {
        Self {
            codec: Box::new(codec),
            presses: Default::default(),
            outgoing: None,
            incoming: Default::default(),
        }
    }
    /// Start when the source widget has crossed its drag threshold. Consumes
    /// this window's most recent press; the backend validates it again.
    pub fn start(
        &mut self,
        window: W,
        payload: P,
        actions: Actions,
    ) -> Result<Effect<P, W>, String> {
        if self.outgoing.is_some() {
            return Err("a native drag is already active".into());
        }
        let bytes = self.codec.encode(&payload)?;
        let source = Source {
            mime: self.codec.mime().into(),
            bytes: Arc::from(bytes),
            actions,
        };
        if !source.valid() {
            return Err("invalid native drag payload".into());
        }
        let gesture = self
            .presses
            .remove(&window)
            .ok_or("no held native press for this window")?;
        self.outgoing = Some(Outgoing {
            window: window.clone(),
            gesture,
            payload,
            started: false,
            actions,
        });
        Ok(Effect::Request {
            window,
            request: Request::Start(gesture, source),
        })
    }
    /// Invalidate an unused press on physical release. An active native drag
    /// continues until protocol completion or cancellation.
    pub fn released(&mut self, window: &W) {
        self.presses.remove(window);
    }
    pub fn dragging(&self) -> bool {
        self.outgoing.as_ref().is_some_and(|source| source.started)
    }
    pub fn pending(&self) -> bool {
        self.outgoing.is_some()
    }
    /// Inform the session if queueing a Start failed synchronously.
    pub fn start_failed(&mut self, window: &W) -> Option<Effect<P, W>> {
        if self
            .outgoing
            .as_ref()
            .is_some_and(|source| &source.window == window && !source.started)
        {
            let source = self.outgoing.take().unwrap();
            Some(Effect::Cancelled {
                window: source.window,
            })
        } else {
            None
        }
    }
    pub fn cancel(&self) -> Option<Effect<P, W>> {
        self.outgoing.as_ref().map(|source| Effect::Request {
            window: source.window.clone(),
            request: Request::Cancel(source.gesture),
        })
    }
    /// Consume one native event. The predicate decides whether the target at
    /// this logical position can accept the MIME payload and chooses its
    /// preferred action. Motion re-runs it as the pointer crosses drop zones.
    pub fn event(
        &mut self,
        window: W,
        event: Event,
        mut accept: impl FnMut(&W, iced_core::Point) -> Option<Action>,
    ) -> Vec<Effect<P, W>> {
        let mut effects = Vec::new();
        match event {
            Event::Gesture(gesture) => {
                self.presses.insert(window, gesture);
            }
            Event::Started(gesture) => {
                if let Some(source) = &mut self.outgoing
                    && source.window == window
                    && source.gesture == gesture
                {
                    source.started = true;
                }
            }
            Event::Rejected(gesture) | Event::Cancelled(gesture) => {
                if self
                    .outgoing
                    .as_ref()
                    .is_some_and(|source| source.window == window && source.gesture == gesture)
                {
                    self.outgoing.take();
                    effects.push(Effect::Cancelled { window });
                }
            }
            Event::Finished { gesture, action } => {
                if self.outgoing.as_ref().is_some_and(|source| {
                    source.window == window && source.gesture == gesture && source.started
                }) {
                    let source = self.outgoing.take().unwrap();
                    if source.actions.contains(action) {
                        effects.push(Effect::Finished {
                            window,
                            payload: source.payload,
                            action,
                        });
                    } else {
                        effects.push(Effect::Cancelled { window });
                    }
                }
            }
            Event::Enter {
                offer,
                position,
                mimes,
            } => {
                let supported = mimes.iter().any(|mime| mime == self.codec.mime());
                let action = supported.then(|| accept(&window, position)).flatten();
                self.incoming.insert(
                    (window.clone(), offer),
                    Incoming {
                        supported,
                        accepted: action.is_some(),
                        action: None,
                        phase: Phase::Entered,
                    },
                );
                effects.push(Self::accept_request(
                    window,
                    offer,
                    action,
                    self.codec.mime(),
                ));
            }
            Event::Motion { offer, position } => {
                if let Some(target) = self.incoming.get_mut(&(window.clone(), offer))
                    && target.phase == Phase::Entered
                {
                    let action = target
                        .supported
                        .then(|| accept(&window, position))
                        .flatten();
                    target.accepted = action.is_some();
                    effects.push(Self::accept_request(
                        window,
                        offer,
                        action,
                        self.codec.mime(),
                    ));
                }
            }
            Event::Leave(offer) => {
                self.incoming.remove(&(window, offer));
            }
            Event::Drop(offer) => {
                if let Some(target) = self.incoming.get_mut(&(window.clone(), offer))
                    && target.phase == Phase::Entered
                {
                    if target.accepted && target.action.is_some() {
                        target.phase = Phase::Receiving;
                        effects.push(Effect::Request {
                            window,
                            request: Request::Receive(offer, self.codec.mime().into()),
                        });
                    } else {
                        self.incoming.remove(&(window.clone(), offer));
                        effects.push(Effect::Request {
                            window,
                            request: Request::Finish(offer, false),
                        });
                    }
                }
            }
            Event::Data {
                offer,
                mime,
                bytes,
                action,
            } => {
                if let Some(target) = self.incoming.get_mut(&(window.clone(), offer))
                    && target.phase == Phase::Receiving
                {
                    let payload = if mime == self.codec.mime()
                        && bytes.len() <= Source::MAX_BYTES
                        && target.action == Some(action)
                    {
                        self.codec.decode(&bytes)
                    } else {
                        Err("invalid native drop payload".into())
                    };
                    match payload {
                        Ok(payload) => {
                            target.phase = Phase::Delivered;
                            effects.push(Effect::Delivery {
                                window,
                                offer,
                                payload,
                                action,
                            });
                        }
                        Err(_) => {
                            self.incoming.remove(&(window.clone(), offer));
                            effects.push(Effect::Request {
                                window: window.clone(),
                                request: Request::Finish(offer, false),
                            });
                            effects.push(Effect::Failed { window, offer });
                        }
                    }
                }
            }
            Event::Failed(offer) => {
                if let Some(target) = self.incoming.remove(&(window.clone(), offer)) {
                    if target.phase != Phase::Entered {
                        effects.push(Effect::Request {
                            window: window.clone(),
                            request: Request::Finish(offer, false),
                        });
                    }
                    effects.push(Effect::Failed { window, offer });
                }
            }
            Event::Action { offer, action } => {
                if let Some(target) = self.incoming.get_mut(&(window, offer)) {
                    target.action = action;
                }
            }
        }
        effects
    }
    fn accept_request(window: W, offer: Offer, action: Option<Action>, mime: &str) -> Effect<P, W> {
        Effect::Request {
            window,
            request: Request::Accept(
                offer,
                action.map(|_| mime.to_owned()),
                Actions::BOTH,
                action.unwrap_or(Action::Copy),
            ),
        }
    }
    /// Call after the application has applied or rejected one Delivery. This
    /// emits at most one finish request; repeated acknowledgements are ignored.
    pub fn applied(&mut self, window: W, offer: Offer, applied: bool) -> Option<Effect<P, W>> {
        let key = (window.clone(), offer);
        if self
            .incoming
            .get(&key)
            .is_some_and(|target| target.phase == Phase::Delivered)
        {
            self.incoming.remove(&key);
            Some(Effect::Request {
                window,
                request: Request::Finish(offer, applied),
            })
        } else {
            None
        }
    }
    /// Remove all state owned by a closed window. The native backend owns
    /// protocol resource cancellation when the window disappears.
    pub fn closed(&mut self, window: &W) -> Option<Effect<P, W>> {
        self.presses.remove(window);
        self.incoming.retain(|(owner, _), _| owner != window);
        if self
            .outgoing
            .as_ref()
            .is_some_and(|source| &source.window == window)
        {
            self.outgoing.take();
            Some(Effect::Cancelled {
                window: window.clone(),
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_outside_source_capabilities_cancels_without_removing_a_move_source() {
        let mut session = Session::new(Text);
        session.event(1u8, Event::Gesture(Gesture(1)), accept);
        session.start(1, "keep".to_owned(), Actions::COPY).unwrap();
        session.event(1, Event::Started(Gesture(1)), accept);
        assert!(matches!(
            session
                .event(
                    1,
                    Event::Finished {
                        gesture: Gesture(1),
                        action: Action::Move
                    },
                    accept
                )
                .as_slice(),
            [Effect::Cancelled { window: 1 }]
        ));
        assert!(!session.pending());
    }
    fn accept(_: &u8, _: iced_core::Point) -> Option<Action> {
        Some(Action::Move)
    }
    fn enter(session: &mut Session<String, u8>, mime: &str) {
        session.event(
            2,
            Event::Enter {
                offer: Offer(10),
                position: iced_core::Point::ORIGIN,
                mimes: vec![mime.into()],
            },
            accept,
        );
        session.event(
            2,
            Event::Action {
                offer: Offer(10),
                action: Some(Action::Move),
            },
            accept,
        );
    }
    #[test]
    fn move_waits_for_decoding_application_and_native_finished_once() {
        let mut session = Session::new(Text);
        session.event(1, Event::Gesture(Gesture(5)), accept);
        let Effect::Request {
            request: Request::Start(_, source),
            ..
        } = session.start(1, "a payload".into(), Actions::BOTH).unwrap()
        else {
            panic!()
        };
        assert!(!session.dragging());
        session.event(1, Event::Started(Gesture(5)), accept);
        assert!(session.dragging());
        enter(&mut session, Text.mime());
        assert!(matches!(
            session.event(2, Event::Drop(Offer(10)), accept).as_slice(),
            [Effect::Request {
                request: Request::Receive(_, _),
                ..
            }]
        ));
        assert!(session.event(2, Event::Drop(Offer(10)), accept).is_empty());
        assert!(session.applied(2, Offer(10), true).is_none());
        let data = Event::Data {
            offer: Offer(10),
            mime: source.mime,
            bytes: source.bytes,
            action: Action::Move,
        };
        assert!(session.event(1, data.clone(), accept).is_empty());
        assert!(
            matches!(session.event(2, data.clone(), accept).as_slice(), [Effect::Delivery { payload, action: Action::Move, .. }] if payload == "a payload")
        );
        assert!(session.event(2, data, accept).is_empty());
        assert!(matches!(
            session.applied(2, Offer(10), true),
            Some(Effect::Request {
                request: Request::Finish(_, true),
                ..
            })
        ));
        assert!(session.applied(2, Offer(10), true).is_none());
        assert!(session.pending());
        assert!(
            session
                .event(
                    2,
                    Event::Finished {
                        gesture: Gesture(5),
                        action: Action::Move
                    },
                    accept
                )
                .is_empty()
        );
        assert!(
            matches!(session.event(1, Event::Finished { gesture: Gesture(5), action: Action::Move }, accept).as_slice(), [Effect::Finished { payload, .. }] if payload == "a payload")
        );
        assert!(
            session
                .event(
                    1,
                    Event::Finished {
                        gesture: Gesture(5),
                        action: Action::Move
                    },
                    accept
                )
                .is_empty()
        );
    }
    #[test]
    fn unsupported_mime_cannot_be_accepted_by_later_motion() {
        let mut session = Session::new(Text);
        enter(&mut session, "image/png");
        assert!(matches!(
            session
                .event(
                    2,
                    Event::Motion {
                        offer: Offer(10),
                        position: iced_core::Point::ORIGIN
                    },
                    accept
                )
                .as_slice(),
            [Effect::Request {
                request: Request::Accept(_, None, _, _),
                ..
            }]
        ));
        assert!(matches!(
            session.event(2, Event::Drop(Offer(10)), accept).as_slice(),
            [Effect::Request {
                request: Request::Finish(_, false),
                ..
            }]
        ));
    }
    #[test]
    fn malformed_drop_rejects_without_delivery_and_cancel_rearms() {
        let mut session = Session::new(Text);
        session.event(1, Event::Gesture(Gesture(5)), accept);
        session.start(1, "value".into(), Actions::BOTH).unwrap();
        session.event(1, Event::Cancelled(Gesture(99)), accept);
        assert!(session.pending());
        session.event(1, Event::Cancelled(Gesture(5)), accept);
        assert!(!session.pending());
        session.event(1, Event::Gesture(Gesture(6)), accept);
        session.start(1, "new".into(), Actions::COPY).unwrap();
        enter(&mut session, Text.mime());
        session.event(2, Event::Drop(Offer(10)), accept);
        let effects = session.event(
            2,
            Event::Data {
                offer: Offer(10),
                mime: Text.mime().into(),
                bytes: Arc::from([255u8]),
                action: Action::Move,
            },
            accept,
        );
        assert!(matches!(
            effects.as_slice(),
            [
                Effect::Request {
                    request: Request::Finish(_, false),
                    ..
                },
                Effect::Failed { .. }
            ]
        ));
        assert!(session.applied(2, Offer(10), true).is_none());
        session.closed(&1);
        assert!(!session.pending());
        session.event(1, Event::Gesture(Gesture(7)), accept);
        session.released(&1);
        assert!(session.start(1, "released".into(), Actions::COPY).is_err());
    }
}
