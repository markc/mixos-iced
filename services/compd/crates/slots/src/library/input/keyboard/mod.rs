pub use crate::shortcut;

pub mod keyboard {
    pub mod key {
        pub use crate::library::input::keyboard::enum_::*;
    }
    pub mod combo {
        pub use crate::library::input::keyboard::combo::*;
    }
    pub mod handler {
        pub use crate::library::input::keyboard::action::ShortcutHandler;
    }
}

#[macro_use]
pub mod action;
pub mod combo;
pub mod enum_;
pub mod format;
