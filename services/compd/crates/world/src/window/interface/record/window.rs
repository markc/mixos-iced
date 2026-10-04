use crate::window::interface::data::data::{WindowData, WindowFullscreen};
use std::cell::RefCell;
use smithay::desktop::{Space, Window};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::compositor::with_states;
use smithay::wayland::seat::WaylandFocus;
use uuid::Uuid;
use dispatcher::wayland::xdg::activation::dispatch::wire::ActivationDetails;
use protocols::window::ident::ident;
use smithay::wayland::xdg_activation::{XdgActivationToken, XdgActivationTokenData};

pub trait LoopWindow {
    fn window_data(&self) -> Option<&WindowData>;
    fn activations(&self) -> Vec<ActivationDetails>;

    fn uuid(&self) -> Option<Uuid>;

    /// The window's fullscreen restore data, if it is currently fullscreen.
    fn fullscreen(&self) -> Option<WindowFullscreen>;
    /// Whether the window is currently fullscreen.
    fn is_fullscreen(&self) -> bool;
    /// Set (or clear, with `None`) the window's fullscreen state.
    fn set_fullscreen(&self, value: Option<WindowFullscreen>);
}

impl LoopWindow for Window {
    fn window_data(&self) -> Option<&WindowData> {
        self.user_data().get::<WindowData>()
    }

    fn uuid(&self) -> Option<Uuid> {
        self.window_data().map(|w| w.UUID)
    }

    fn fullscreen(&self) -> Option<WindowFullscreen> {
        self.user_data()
            .get::<RefCell<Option<WindowFullscreen>>>()
            .and_then(|cell| *cell.borrow())
    }

    fn is_fullscreen(&self) -> bool {
        self.fullscreen().is_some()
    }

    fn set_fullscreen(&self, value: Option<WindowFullscreen>) {
        self.user_data()
            .insert_if_missing(|| RefCell::<Option<WindowFullscreen>>::new(None));
        *self
            .user_data()
            .get::<RefCell<Option<WindowFullscreen>>>()
            .expect("fullscreen cell just inserted")
            .borrow_mut() = value;
    }

    fn activations(&self) -> Vec<ActivationDetails> {
        // X11 declares the same thing through `_NET_STARTUP_ID` — startup notification
        // is the protocol xdg-activation replaced, and the token the compositor hands a launch is
        // exported under BOTH names (`XDG_ACTIVATION_TOKEN` and `DESKTOP_STARTUP_ID`),
        // so an X11 client that sets the property is naming the very token we minted.
        //
        // Reading it here rather than leaving X11 to the `/proc` fallback matters: the
        // env route needs `_NET_WM_PID`, a readable `/proc/<pid>/environ` and the var
        // to have survived into it, while this is one property the X server already
        // tracked for us. The env route stays as the fallback for clients that consume
        // the variable without setting the property.
        if let Some(startup_id) = ident::x11_startup_id(self) {
            let token = XdgActivationToken::from(startup_id);
            // No `XdgActivationTokenData` was ever created for it — the client did not
            // go through `xdg_activation_v1` — so the correlation token is the whole
            // payload, which is all any caller reads.
            return vec![ActivationDetails { token, token_data: XdgActivationTokenData::default() }];
        }
        // CHECK: See WindowData re-insertion logic which explains how surface may outlive window and be created with new window.
        let Some(surface) = ident::xdg_surface(self) else { return vec![] };
        dispatcher::wayland::xdg::activation::dispatch::wire::activations(&surface)
    }
}
