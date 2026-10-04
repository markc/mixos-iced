use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::input_method::InputMethodManagerState;
use smithay::wayland::text_input::TextInputManagerState;
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;

pub struct TextInput {
    pub input_method_manager_state: InputMethodManagerState,
    pub text_input_manager_state: TextInputManagerState,
    // Required alongside input-method-v2: IMEs (fcitx5/ibus) use virtual-keyboard-v1 to
    // inject keys back into the focused app, and their waylandim frontend refuses to enable
    // the native input-method protocol unless this manager is also advertised.
    pub virtual_keyboard_manager_state: VirtualKeyboardManagerState,
}
