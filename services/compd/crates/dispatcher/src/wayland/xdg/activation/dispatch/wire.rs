use smithay::wayland::xdg_activation::XdgActivationState;
use crate::state::state::{Dispatch, DispatchWire};

pub use protocols::xdg::activation::request::{
    activations, request_activation, ActivationDetails, ActivationLog,
};

pub fn activation_state(
    dispatch: &mut Dispatch,
) -> &mut XdgActivationState {
    &mut dispatch.xdg_activation.xdg_activation
}
