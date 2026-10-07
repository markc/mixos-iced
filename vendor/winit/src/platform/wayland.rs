//! # Wayland
//!
//! **Note:** Windows don't appear on Wayland until you draw/present to them.
//!
//! By default, Winit loads system libraries using `dlopen`. This can be
//! disabled by disabling the `"wayland-dlopen"` cargo feature.
//!
//! ## Client-side decorations
//!
//! Winit provides client-side decorations by default, but the behaviour can
//! be controlled with the following feature flags:
//!
//! * `wayland-csd-adwaita` (default).
//! * `wayland-csd-adwaita-crossfont`.
//! * `wayland-csd-adwaita-notitle`.
use crate::event_loop::{ActiveEventLoop, EventLoop, EventLoopBuilder};
use crate::monitor::MonitorHandle;
use crate::window::{Window, WindowAttributes};

pub use crate::window::Theme;

/// Additional methods on [`ActiveEventLoop`] that are specific to Wayland.
pub trait ActiveEventLoopExtWayland {
    /// True if the [`ActiveEventLoop`] uses Wayland.
    fn is_wayland(&self) -> bool;
}

impl ActiveEventLoopExtWayland for ActiveEventLoop {
    #[inline]
    fn is_wayland(&self) -> bool {
        self.p.is_wayland()
    }
}

/// Additional methods on [`EventLoop`] that are specific to Wayland.
pub trait EventLoopExtWayland {
    /// True if the [`EventLoop`] uses Wayland.
    fn is_wayland(&self) -> bool;
}

impl<T: 'static> EventLoopExtWayland for EventLoop<T> {
    #[inline]
    fn is_wayland(&self) -> bool {
        self.event_loop.is_wayland()
    }
}

/// Additional methods on [`EventLoopBuilder`] that are specific to Wayland.
pub trait EventLoopBuilderExtWayland {
    /// Force using Wayland.
    fn with_wayland(&mut self) -> &mut Self;

    /// Whether to allow the event loop to be created off of the main thread.
    ///
    /// By default, the window is only allowed to be created on the main
    /// thread, to make platform compatibility easier.
    fn with_any_thread(&mut self, any_thread: bool) -> &mut Self;
}

impl<T> EventLoopBuilderExtWayland for EventLoopBuilder<T> {
    #[inline]
    fn with_wayland(&mut self) -> &mut Self {
        self.platform_specific.forced_backend = Some(crate::platform_impl::Backend::Wayland);
        self
    }

    #[inline]
    fn with_any_thread(&mut self, any_thread: bool) -> &mut Self {
        self.platform_specific.any_thread = any_thread;
        self
    }
}

/// Additional methods on [`Window`] that are specific to Wayland.
pub trait WindowExtWayland {
    /// Request one recovery redraw after a recoverable presentation failure
    /// following `pre_present_notify`. Retains and reuses the outstanding
    /// pacing callback; creates no callback, commit or presentation receipt.
    fn request_redraw_after_present_failure(&self);
    /// Request feedback for the immediately following commit on this window's
    /// real surface. Call synchronously after painting and before buffer commit.
    /// Outstanding native objects and retained receipt copies share a bounded
    /// budget of eight per window and 128 per process.
    fn request_presentation_feedback(
        &self,
    ) -> Result<crate::presentation::PresentationId, crate::presentation::PresentationError>;
    /// Queue a native drag from a still-held press on this window. The event
    /// loop validates the token again before sending wl_data_device.start_drag.
    fn start_drag(
        &self,
        gesture: crate::drag::Gesture,
        source: crate::drag::Source,
    ) -> Result<(), crate::drag::Error>;
    /// Negotiate an offered MIME type and supported actions, or reject it.
    fn accept_drag_offer(
        &self,
        offer: crate::drag::Offer,
        mime: Option<String>,
        actions: crate::drag::Actions,
        preferred: crate::drag::Action,
    ) -> Result<(), crate::drag::Error>;
    /// Receive a dropped offer through its native transfer pipe.
    fn receive_drag_offer(
        &self,
        offer: crate::drag::Offer,
        mime: String,
    ) -> Result<(), crate::drag::Error>;
    /// Complete after applying the decoded bytes; false rejects the drop.
    fn finish_drag_offer(
        &self,
        offer: crate::drag::Offer,
        applied: bool,
    ) -> Result<(), crate::drag::Error>;
    /// Cancel this source, including any transfers in progress.
    fn cancel_drag(&self, gesture: crate::drag::Gesture) -> Result<(), crate::drag::Error>;
}

impl WindowExtWayland for Window {
    fn request_redraw_after_present_failure(&self) {
        match &self.window {
            crate::platform_impl::Window::Wayland(window) => window.request_redraw_after_present_failure(),
            #[cfg(x11_platform)]
            _ => self.request_redraw(),
        }
    }
    fn request_presentation_feedback(
        &self,
    ) -> Result<crate::presentation::PresentationId, crate::presentation::PresentationError> {
        match &self.window {
            crate::platform_impl::Window::Wayland(window) => window.request_presentation_feedback(),
            #[cfg(x11_platform)]
            _ => Err(crate::presentation::PresentationError::Unsupported),
        }
    }
    fn start_drag(
        &self,
        gesture: crate::drag::Gesture,
        source: crate::drag::Source,
    ) -> Result<(), crate::drag::Error> {
        if !source.valid() {
            return Err(crate::drag::Error::Invalid);
        }
        self.queue_drag(crate::platform_impl::wayland::data_device::Request::Start(gesture, source))
    }
    fn accept_drag_offer(
        &self,
        offer: crate::drag::Offer,
        mime: Option<String>,
        actions: crate::drag::Actions,
        preferred: crate::drag::Action,
    ) -> Result<(), crate::drag::Error> {
        if mime
            .as_ref()
            .is_some_and(|m| m.is_empty() || m.len() > 255 || m.chars().any(|c| c.is_control()))
            || !actions.contains(preferred)
        {
            return Err(crate::drag::Error::Invalid);
        }
        self.queue_drag(crate::platform_impl::wayland::data_device::Request::Accept(
            offer, mime, actions, preferred,
        ))
    }
    fn receive_drag_offer(
        &self,
        offer: crate::drag::Offer,
        mime: String,
    ) -> Result<(), crate::drag::Error> {
        self.queue_drag(crate::platform_impl::wayland::data_device::Request::Receive(offer, mime))
    }
    fn finish_drag_offer(
        &self,
        offer: crate::drag::Offer,
        applied: bool,
    ) -> Result<(), crate::drag::Error> {
        self.queue_drag(crate::platform_impl::wayland::data_device::Request::Finish(offer, applied))
    }
    fn cancel_drag(&self, gesture: crate::drag::Gesture) -> Result<(), crate::drag::Error> {
        self.queue_drag(crate::platform_impl::wayland::data_device::Request::Cancel(gesture))
    }
}

impl Window {
    fn queue_drag(
        &self,
        request: crate::platform_impl::wayland::data_device::Request,
    ) -> Result<(), crate::drag::Error> {
        match &self.window {
            crate::platform_impl::Window::Wayland(window) => window.queue_drag(request),
            #[cfg(x11_platform)]
            _ => Err(crate::drag::Error::Unsupported),
        }
    }
}

/// Additional methods on [`WindowAttributes`] that are specific to Wayland.
pub trait WindowAttributesExtWayland {
    /// Build window with the given name.
    ///
    /// The `general` name sets an application ID, which should match the `.desktop`
    /// file distributed with your program. The `instance` is a `no-op`.
    ///
    /// For details about application ID conventions, see the
    /// [Desktop Entry Spec](https://specifications.freedesktop.org/desktop-entry-spec/desktop-entry-spec-latest.html#desktop-file-id)
    fn with_name(self, general: impl Into<String>, instance: impl Into<String>) -> Self;
}

impl WindowAttributesExtWayland for WindowAttributes {
    #[inline]
    fn with_name(mut self, general: impl Into<String>, instance: impl Into<String>) -> Self {
        self.platform_specific.name =
            Some(crate::platform_impl::ApplicationName::new(general.into(), instance.into()));
        self
    }
}

/// Additional methods on `MonitorHandle` that are specific to Wayland.
pub trait MonitorHandleExtWayland {
    /// Returns the inner identifier of the monitor.
    fn native_id(&self) -> u32;
}

impl MonitorHandleExtWayland for MonitorHandle {
    #[inline]
    fn native_id(&self) -> u32 {
        self.inner.native_identifier()
    }
}
