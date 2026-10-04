//! Quoin's panel holders on the compd host.
//!
//! policy `panel` is the per-edge state machine; this is what the host
//! keeps around it between passes, plus the CONCEAL marker the engine reads.
//! policy-host `panel` runs the passes.
//!
//! The marker: a layer surface with a conceal enforced on it (its owner left a
//! liveness probe unanswered, or a conceal went unapplied past its grace) is
//! marked on its own surface data, so the three engine points that must
//! honour it read the surface alone, with no `Loop`:
//! - the layer draw loop (frames `scene::layershell`) skips it;
//! - layer hit-testing (world `surface::interface::core::hit`) skips it;
//! - layer frame callbacks (frames `draw::present::callbacks`) skip it.
//!
//! seat's exclusive-keyboard lookup skips it too (a stopped owner keeps no
//! keyboard grab).

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use policy::panel::{PanelHolders, PopupRestore};
use surfaces::SurfaceId;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::Serial;
use smithay::wayland::compositor::with_states;

/// One edge's holders, keyed `(output name, edge)`.
pub type Holders = PanelHolders<ClientId, Serial>;

#[derive(Debug, Default)]
pub struct Panels {
    pub holders: BTreeMap<(String, String), Holders>,
    /// The union of every edge's enforced layers, as last applied.
    pub enforced: BTreeSet<SurfaceId>,
    /// Owners that left a probe unanswered.
    pub stalled_owners: Vec<ClientId>,
    /// Focus restoration per held popup layer (by surface id).
    pub popup_restores: BTreeMap<u64, PopupRestore>,
    /// Button and key presses since the last pass, by the client they
    /// reached.
    pub user_input: Vec<Option<ClientId>>,
    /// Layer `ack_configure`s since the last pass: the layer and the serial.
    pub layer_acks: Vec<(SurfaceId, Serial)>,
    /// Keyboard focus changes since the last pass, `(to, from)` by surface id.
    pub focus_changes: Vec<(Option<u64>, Option<u64>)>,
    /// The last keyboard focus change, `(to, from)`.
    pub last_focus_change: Option<(Option<u64>, Option<u64>)>,
    /// `panel.command`s owed: `(output, edge, surface, reveal)`.
    pub commands: Vec<(String, String, String, bool)>,
}

impl Panels {
    pub fn is_empty(&self) -> bool {
        self.holders.is_empty()
    }
}

/// The conceal marker on a surface's data.
struct Concealed(Cell<bool>);

/// Whether enforcement is hiding this layer surface and excluding it from input.
pub fn concealed(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        states
            .data_map
            .get::<Concealed>()
            .is_some_and(|concealed| concealed.0.get())
    })
}

/// Mark (or unmark) a layer surface concealed.
pub fn set_concealed(surface: &WlSurface, on: bool) {
    with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing(|| Concealed(Cell::new(false)));
        if let Some(concealed) = states.data_map.get::<Concealed>() {
            concealed.0.set(on);
        }
    });
}
