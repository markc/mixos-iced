//! comp-service: compd's native Bus transport for the `comp` service, the
//! `comp.*` Bus surface served from inside compd.
//!
//! Threads and channels:
//!
//! ```text
//!  noded ──ws── [compd-port thread: current-thread tokio]
//!                 worker_loop ── dispatch_incoming (classify, permits)
//!                    │  responders (JoinSet) ── reply_loop ── respond_parts
//!                    │  publisher_loop ◀── ObservationOutbox (crossbeam)
//!                    ▼                              ▲
//!          CommandSender ──bounded 16──▶ CommandSource      ObservationProducer
//!            (waker on every send)           │                    │
//!                                  [engine loop] PortService::service(&mut impl CompEngine)
//! ```
//!
//! - [`port::prepare`] builds both halves; [`port::PortStarter::start`]
//!   spawns the worker thread; the engine keeps [`port::PortWiring`].
//! - The engine is woken by the [`channel::Waker`] it supplies (a calloop
//!   ping, an eventfd), never by a poll, and answers inside
//!   [`service::PortService::service`] through the [`service::CompEngine`]
//!   trait, which it implements with policy decisions.
//! - Observation records go into [`outbox::ObservationProducer`]; the
//!   publisher task frames them as Bus messages with gap recovery.

pub mod channel;
pub mod outbox;
pub mod port;
pub mod service;

pub use channel::{Waker, no_waker};
pub use outbox::{ObservationOutbox, ObservationProducer};
pub use port::{
    PortIdentity, PortStarter, PortWiring, PortWorker, QueueFull, default_noded_url, prepare,
};
pub use service::{CompEngine, LongReply, PortContext, PortService};
