use smithay::desktop::PopupManager;
use crate::state::state::DispatchWire;
use protocols::popup::state::PopupState;

pub fn new<I: DispatchWire>() -> PopupState {
    let popup = PopupManager::default();

    return PopupState { state: popup };
}
