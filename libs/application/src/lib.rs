// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native window hosting shared by desktop applications.
//!
//! The host owns the take-once bootstrap, bounded task executor, window defaults
//! and renderer feature selection. Applications supply their reducer, view and
//! subscriptions, and retain ownership of service lifetimes and shutdown.
//! Portable widgets belong in `toolkit`; renderer-specific adapters belong here.

use std::cell::RefCell;

pub use iced;
pub mod message;
pub mod native_queue;
pub use iced_runtime as runtime;
/// Native app tests use the pinned simulator through their shared host.
#[cfg(feature = "test-support")]
pub use iced_test as test;
#[cfg(feature = "tiny-skia")]
pub use iced_tiny_skia as cpu;

/// Ordinary native applications keep the software renderer when unrelated
/// workspace consumers also enable iced's GPU renderer. Hosts may still
/// supply an explicit renderer to [`Element`] and [`start`].
#[cfg(feature = "tiny-skia")]
pub type Renderer = cpu::Renderer;
#[cfg(not(feature = "tiny-skia"))]
pub type Renderer = iced::Renderer;

pub type Element<'a, Message, Theme = iced::Theme, Backend = Renderer> =
    iced::Element<'a, Message, Theme, Backend>;

/// Renderer-selected return types for native host factories. Re-exported
/// widget builders remain generic and infer the enclosing Element's backend.
pub mod widget {
    pub use iced::widget::*;

    pub type Text<'a, Theme = iced::Theme, Backend = super::Renderer> =
        iced::widget::Text<'a, Theme, Backend>;
    pub type Button<'a, Message, Theme = iced::Theme, Backend = super::Renderer> =
        iced::widget::Button<'a, Message, Theme, Backend>;
}

#[cfg(feature = "settings")]
pub mod presentation;

#[cfg(feature = "native-grid")]
pub mod native_grid;

#[cfg(feature = "wgpu")]
pub mod gpu_grid;

/// Native window configuration. Close deferral lets applications finish saving
/// before their reducer explicitly exits; it does not install a shutdown hook.
pub struct Window {
    settings: iced::window::Settings,
    font: iced::Font,
}

impl Window {
    pub fn new(id: impl Into<String>, size: iced::Size, font: iced::Font) -> Self {
        Self {
            settings: iced::window::Settings {
                size,
                platform_specific: iced::window::settings::PlatformSpecific {
                    application_id: id.into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            font,
        }
    }

    pub fn minimum(mut self, size: iced::Size) -> Self {
        self.settings.min_size = Some(size);
        self
    }

    pub fn defer_close(mut self) -> Self {
        self.settings.exit_on_close_request = false;
        self
    }
}

fn boot_once<State, Message>(
    initial: (State, iced::Task<Message>),
) -> impl Fn() -> (State, iced::Task<Message>) {
    let initial = RefCell::new(Some(initial));
    move || initial.borrow_mut().take().expect("application boots once")
}

/// Prepare a native application without cloning its initial state or spawning
/// an executor per CPU. Configuration of title, theme, style and subscriptions
/// remains on the returned builder; `.run()` enters the native event loop.
pub fn start<State, Message, Theme, Renderer>(
    initial: (State, iced::Task<Message>),
    update: impl iced::application::UpdateFn<State, Message>,
    view: impl for<'a> iced::application::ViewFn<'a, State, Message, Theme, Renderer>,
    window: Window,
) -> iced::application::Application<
    impl iced::Program<State = State, Message = Message, Theme = Theme>,
>
where
    State: 'static,
    Message: Send + 'static,
    Theme: iced::theme::Base,
    Renderer: iced_program::Renderer,
{
    iced::application(boot_once(initial), update, view)
        .executor::<SingleThread>()
        .default_font(window.font)
        .window(window.settings)
}

/// One asynchronous task worker. Service workers and PTYs remain owned by their
/// applications; the host never adds an idle timer or a service runtime.
struct SingleThread(iced::futures::executor::ThreadPool);

impl iced::Executor for SingleThread {
    fn new() -> Result<Self, iced::futures::io::Error> {
        iced::futures::executor::ThreadPool::builder()
            .pool_size(1)
            .name_prefix("application-task")
            .create()
            .map(Self)
    }

    fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        self.0.spawn_ok(future);
    }

    fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
        iced::futures::executor::block_on(future)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::Executor;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn bootstrap_moves_nonclone_state_and_never_duplicates_it() {
        struct State(Arc<AtomicUsize>);
        impl Drop for State {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let boot = boot_once((State(Arc::clone(&drops)), iced::Task::<()>::none()));
        let (state, _) = boot();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(&boot)).is_err());
        drop(state);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn close_deferral_is_explicit_and_window_identity_is_preserved() {
        let window = Window::new(
            "example.editor",
            iced::Size::new(800.0, 600.0),
            iced::Font::DEFAULT,
        );
        assert!(window.settings.exit_on_close_request);
        let window = window.minimum(iced::Size::new(300.0, 200.0)).defer_close();
        assert!(!window.settings.exit_on_close_request);
        assert_eq!(
            window.settings.platform_specific.application_id,
            "example.editor"
        );
        assert_eq!(
            window.settings.min_size,
            Some(iced::Size::new(300.0, 200.0))
        );
    }

    #[test]
    fn asynchronous_tasks_execute_off_the_calling_thread() {
        let executor = SingleThread::new().unwrap();
        let caller = std::thread::current().id();
        let (send, receive) = std::sync::mpsc::channel();
        executor.spawn(async move {
            send.send(std::thread::current().id()).unwrap();
        });
        assert_ne!(
            caller,
            receive
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
        );
        assert_eq!(executor.block_on(async { 42 }), 42);
    }
}
