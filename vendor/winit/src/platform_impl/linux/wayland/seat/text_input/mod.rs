use std::ops::Deref;

use sctk::globals::GlobalData;
use sctk::reexports::client::{Connection, Proxy, QueueHandle};

use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Dispatch, delegate_dispatch};
use sctk::reexports::protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use sctk::reexports::protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    ContentHint, ContentPurpose, Event as TextInputEvent, ZwpTextInputV3,
};

use crate::event::{Ime, WindowEvent};
use crate::platform_impl::wayland;
use crate::platform_impl::wayland::state::WinitState;
use crate::window::ImePurpose;

pub struct TextInputState {
    text_input_manager: ZwpTextInputManagerV3,
}

impl TextInputState {
    pub fn new(
        globals: &GlobalList,
        queue_handle: &QueueHandle<WinitState>,
    ) -> Result<Self, BindError> {
        let text_input_manager = globals.bind(queue_handle, 1..=1, GlobalData)?;
        Ok(Self { text_input_manager })
    }
}

impl Deref for TextInputState {
    type Target = ZwpTextInputManagerV3;

    fn deref(&self) -> &Self::Target {
        &self.text_input_manager
    }
}

impl Dispatch<ZwpTextInputManagerV3, GlobalData, WinitState> for TextInputState {
    fn event(
        _state: &mut WinitState,
        _proxy: &ZwpTextInputManagerV3,
        _event: <ZwpTextInputManagerV3 as Proxy>::Event,
        _data: &GlobalData,
        _conn: &Connection,
        _qhandle: &QueueHandle<WinitState>,
    ) {
    }
}

impl Dispatch<ZwpTextInputV3, TextInputData, WinitState> for TextInputState {
    fn event(
        state: &mut WinitState,
        text_input: &ZwpTextInputV3,
        event: <ZwpTextInputV3 as Proxy>::Event,
        data: &TextInputData,
        _conn: &Connection,
        _qhandle: &QueueHandle<WinitState>,
    ) {
        let windows = state.windows.get_mut();
        let mut text_input_data = data.inner.lock().unwrap();
        match event {
            TextInputEvent::Enter { surface } => {
                let window_id = wayland::make_wid(&surface);
                text_input_data.surface = Some(surface);
                text_input_data.retire_pending();
                drop(text_input_data);

                let mut window = match windows.get(&window_id) {
                    Some(window) => window.lock().unwrap(),
                    None => return,
                };

                if window.ime_allowed() {
                    text_input.enable();
                    text_input.set_content_type_by_purpose(window.ime_purpose());
                    text_input.commit_tracked(Some(true));
                    state.events_sink.push_window_event(WindowEvent::Ime(Ime::Enabled), window_id);
                }

                window.text_input_entered(text_input);
            },
            TextInputEvent::Leave { surface } => {
                text_input_data.surface = None;

                // Always issue a disable.
                text_input.disable();
                text_input_data.commit_epoch(Some(false));
                text_input.commit();
                drop(text_input_data);

                let window_id = wayland::make_wid(&surface);

                // XXX this check is essential, because `leave` could have a
                // reference to nil surface...
                let mut window = match windows.get(&window_id) {
                    Some(window) => window.lock().unwrap(),
                    None => return,
                };

                window.text_input_left(text_input);

                state.events_sink.push_window_event(WindowEvent::Ime(Ime::Disabled), window_id);
            },
            TextInputEvent::PreeditString { text, cursor_begin, cursor_end } => {
                let text = text.unwrap_or_default();
                let cursor_begin = usize::try_from(cursor_begin)
                    .ok()
                    .and_then(|idx| text.is_char_boundary(idx).then_some(idx));
                let cursor_end = usize::try_from(cursor_end)
                    .ok()
                    .and_then(|idx| text.is_char_boundary(idx).then_some(idx));

                text_input_data.pending_preedit = Some(Preedit { text, cursor_begin, cursor_end })
            },
            TextInputEvent::CommitString { text } => {
                text_input_data.pending_preedit = None;
                text_input_data.pending_commit = text;
            },
            TextInputEvent::Done { serial } => {
                let Some((commit, preedit)) = text_input_data.take_pending(serial) else {
                    return;
                };
                let window_id = match text_input_data.surface.as_ref() {
                    Some(surface) => wayland::make_wid(surface),
                    None => return,
                };

                // Clear preedit at the start of `Done`.
                state.events_sink.push_window_event(
                    WindowEvent::Ime(Ime::Preedit(String::new(), None)),
                    window_id,
                );

                // Send `Commit`.
                if let Some(text) = commit {
                    state
                        .events_sink
                        .push_window_event(WindowEvent::Ime(Ime::Commit(text)), window_id);
                }

                // Send preedit.
                if let Some(preedit) = preedit {
                    let cursor_range =
                        preedit.cursor_begin.map(|b| (b, preedit.cursor_end.unwrap_or(b)));

                    state.events_sink.push_window_event(
                        WindowEvent::Ime(Ime::Preedit(preedit.text, cursor_range)),
                        window_id,
                    );
                }
            },
            TextInputEvent::DeleteSurroundingText { .. } => {
                // Not handled.
            },
            _ => {},
        }
    }
}

pub trait ZwpTextInputV3Ext {
    fn set_content_type_by_purpose(&self, purpose: ImePurpose);
    fn commit_tracked(&self, enabled: Option<bool>);
}

impl ZwpTextInputV3Ext for ZwpTextInputV3 {
    fn commit_tracked(&self, enabled: Option<bool>) {
        let data = self.data::<TextInputData>().expect("owned text input data");
        data.inner.lock().unwrap().commit_epoch(enabled);
        self.commit();
    }
    fn set_content_type_by_purpose(&self, purpose: ImePurpose) {
        let (hint, purpose) = match purpose {
            ImePurpose::Normal => (ContentHint::None, ContentPurpose::Normal),
            ImePurpose::Password => (ContentHint::SensitiveData, ContentPurpose::Password),
            ImePurpose::Terminal => (ContentHint::None, ContentPurpose::Terminal),
        };
        self.set_content_type(hint, purpose);
    }
}

/// The Data associated with the text input.
#[derive(Default)]
pub struct TextInputData {
    inner: std::sync::Mutex<TextInputDataInner>,
}

#[derive(Default)]
pub struct TextInputDataInner {
    commit_serial: u32,
    enabled_at: Option<u32>,
    /// The `WlSurface` we're performing input to.
    surface: Option<WlSurface>,

    /// The commit to submit on `done`.
    pending_commit: Option<String>,

    /// The preedit to submit on `done`.
    pending_preedit: Option<Preedit>,
}

impl TextInputDataInner {
    fn retire_pending(&mut self) {
        self.pending_commit = None;
        self.pending_preedit = None;
    }

    fn commit_epoch(&mut self, enabled: Option<bool>) {
        self.commit_serial = self.commit_serial.wrapping_add(1);
        if let Some(enabled) = enabled {
            self.enabled_at = enabled.then_some(self.commit_serial);
            self.retire_pending();
        }
    }

    fn take_pending(&mut self, serial: u32) -> Option<(Option<String>, Option<Preedit>)> {
        // Done carries the compositor's commit count. Older cursor/content
        // updates within this enable context still apply, as the protocol
        // requires. A batch from before the latest enable never does.
        let valid = self.enabled_at.is_some_and(|first| {
            serial.wrapping_sub(first) <= self.commit_serial.wrapping_sub(first)
        });
        let pending = (self.pending_commit.take(), self.pending_preedit.take());
        valid.then_some(pending)
    }
}

#[cfg(test)]
mod epoch_tests {
    use super::*;

    #[test]
    fn old_done_cannot_reach_replacement_after_synthetic_disabled() {
        let mut data = TextInputDataInner::default();
        data.commit_epoch(Some(true));
        let old = data.commit_serial;
        data.pending_commit = Some("old owner".into());
        data.commit_epoch(Some(false));
        assert!(data.take_pending(old).is_none());
        data.commit_epoch(Some(true));
        data.pending_commit = Some("queued old owner".into());
        assert!(data.take_pending(old).is_none());
        data.pending_commit = Some("new owner".into());
        assert_eq!(data.take_pending(data.commit_serial).unwrap().0.as_deref(), Some("new owner"));
    }

    #[test]
    fn older_cursor_commit_in_same_context_is_valid() {
        let mut data = TextInputDataInner::default();
        data.commit_epoch(Some(true));
        let active = data.commit_serial;
        data.commit_epoch(None);
        data.pending_commit = Some("valid".into());
        assert_eq!(data.take_pending(active).unwrap().0.as_deref(), Some("valid"));
        assert!(data.take_pending(data.commit_serial.wrapping_add(1)).is_none());
    }

    #[test]
    fn epochs_and_commit_counts_survive_serial_wrap() {
        let mut data = TextInputDataInner { commit_serial: u32::MAX - 1, ..Default::default() };
        data.commit_epoch(Some(true));
        data.commit_epoch(None);
        assert!(data.take_pending(u32::MAX).is_some());
        assert!(data.take_pending(0).is_some());
        data.commit_epoch(Some(false));
        data.commit_epoch(Some(true));
        assert!(data.take_pending(u32::MAX).is_none());
        assert!(data.take_pending(0).is_none());
        assert!(data.take_pending(2).is_some());
    }
}

/// The state of the preedit.
struct Preedit {
    text: String,
    cursor_begin: Option<usize>,
    cursor_end: Option<usize>,
}

delegate_dispatch!(WinitState: [ZwpTextInputManagerV3: GlobalData] => TextInputState);
delegate_dispatch!(WinitState: [ZwpTextInputV3: TextInputData] => TextInputState);
