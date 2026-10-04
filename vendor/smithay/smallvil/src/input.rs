use smithay::{
    backend::input::{
        KeyState,
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    },
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::SERIAL_COUNTER,
};

use smithay::input::keyboard::{keysyms};


// This is the source of truth for Smithay keys

use crate::state::Smallvil;



impl Smallvil {
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => {
                let serial = SERIAL_COUNTER.next_serial();
                let time = Event::time_msec(&event);
                let key_state = event.state(); // Capture the state (Pressed/Released) here
                self.seat.get_keyboard().unwrap().input::<(), _>(
                    self,
                    event.key_code(),
                    key_state,
                    serial,
                    time,
                    |state, _modifiers, handle| {
                        // 'state' is &mut Smallvil
                        // 'handle' is the KeysymHandle

                        if key_state == KeyState::Pressed {
                            let keysym = handle.modified_sym();
                            let speed = 100.0 / state.zoom;

                            // Match against the constants in smithay::input::keyboard::keysyms
                            if keysym == keysyms::KEY_Left.into() {
                                state.camera_pos.x -= speed;
                                return FilterResult::Intercept(());
                            } else if keysym == keysyms::KEY_Right.into() {
                                state.camera_pos.x += speed;
                                return FilterResult::Intercept(());
                            } else if keysym == keysyms::KEY_Up.into() {
                                state.camera_pos.y -= speed;
                                return FilterResult::Intercept(());
                            } else if keysym == keysyms::KEY_Down.into() {
                                state.camera_pos.y += speed;
                                return FilterResult::Intercept(());
                            } else if keysym == keysyms::KEY_plus.into() || keysym == keysyms::KEY_equal.into() || keysym == keysyms::KEY_KP_Add.into() {
                                state.zoom *= 1.1;
                                return FilterResult::Intercept(());
                            } else if keysym == keysyms::KEY_minus.into() || keysym == keysyms::KEY_KP_Subtract.into() {
                                state.zoom /= 1.1;
                                return FilterResult::Intercept(());
                            }
                        }

                        FilterResult::Forward
                    },
                );
            }

            InputEvent::PointerMotionAbsolute { event, .. } => {
                let output = self.space.outputs().next().unwrap();
                let output_geo = self.space.output_geometry(output).unwrap();

                // 1. Get the raw physical screen position
                let screen_pos = event.position_transformed(output_geo.size) + output_geo.loc.to_f64();

                // --- NEW: DRAG TO PAN LOGIC ---
                if self.is_panning {
                    // Calculate how many physical pixels the mouse moved since the last frame
                    let dx = screen_pos.x - self.last_screen_pos.x;
                    let dy = screen_pos.y - self.last_screen_pos.y;

                    // Move the camera in the opposite direction of the drag, scaled by zoom
                    self.camera_pos.x -= dx / self.zoom;
                    self.camera_pos.y -= dy / self.zoom;
                }
                // Save the position for the next frame's delta calculation

                self.last_screen_pos = (screen_pos.x, screen_pos.y).into();
                // ------------------------------

                // 2. Apply the Inverse Math to find the Logical World Coordinate
                let size = output_geo.size.to_f64();
                let centered_x = screen_pos.x - (size.w / 2.0);
                let centered_y = screen_pos.y - (size.h / 2.0);

                let unscaled_x = centered_x / self.zoom;
                let unscaled_y = centered_y / self.zoom;

                let logical_pos = smithay::utils::Point::from((
                    unscaled_x + self.camera_pos.x,
                    unscaled_y + self.camera_pos.y,
                ));

                let serial = SERIAL_COUNTER.next_serial();
                let pointer = self.seat.get_pointer().unwrap();

                // 3. Use the logical position for hit detection
                let under = self.surface_under(logical_pos);

                // 4. Update Smithay's pointer state
                pointer.motion(
                    self,
                    under,
                    &MotionEvent {
                        location: logical_pos,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerButton { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                let keyboard = self.seat.get_keyboard().unwrap();

                let serial = SERIAL_COUNTER.next_serial();
                let button = event.button_code();
                let button_state = event.state();

                if ButtonState::Pressed == button_state && !pointer.is_grabbed() {
                    if let Some((window, _loc)) = self
                        .space
                        .element_under(pointer.current_location())
                        .map(|(w, l)| (w.clone(), l))
                    {
                        // User clicked a window
                        self.space.raise_element(&window, true);
                        keyboard.set_focus(
                            self,
                            Some(window.toplevel().unwrap().wl_surface().clone()),
                            serial,
                        );
                        self.space.elements().for_each(|w| {
                            w.set_activated(w == &window);
                            w.toplevel().unwrap().send_pending_configure();
                        });

                        // // 3. --- NEW: Update the visual Activation State ---
                        // self.space.elements().for_each(|w| {
                        //     // Set true for the clicked window, false for all others
                        //     w.toplevel().unwrap().send_pending_configure();
                        // });
                    } else {
                        // --- NEW: START PANNING ---
                        // User clicked on the empty canvas
                        self.is_panning = true;

                        // Deactivates all focuses
                        self.space.elements().for_each(|window| {
                            window.set_activated(false);
                            window.toplevel().unwrap().send_pending_configure();
                        });

                        keyboard.set_focus(self, Option::<WlSurface>::None, serial);
                    }
                } else if ButtonState::Released == button_state {
                    // --- NEW: STOP PANNING ---
                    self.is_panning = false;
                }

                pointer.button(
                    self,
                    &ButtonEvent {
                        button,
                        state: button_state,
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event, .. } => {
                let pointer = self.seat.get_pointer().unwrap();

                // --- NEW: SCROLL TO ZOOM LOGIC ---
                let is_over_window = self.space.element_under(pointer.current_location()).is_some();

                if !is_over_window {
                    let vertical_amount = event
                        .amount(Axis::Vertical)
                        .unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0));

                    if vertical_amount != 0.0 {
                        // 1. Capture the logical world coordinate under the mouse
                        let cursor_logical = pointer.current_location();
                        let old_zoom = self.zoom;

                        // 2. Apply the zoom multiplier
                        if vertical_amount < 0.0 {
                            self.zoom *= 1.1; // Zoom in
                        } else {
                            self.zoom /= 1.1; // Zoom out
                        }

                        // 3. Shift the camera to keep the cursor over the exact same logical point
                        let zoom_ratio = old_zoom / self.zoom;

                        self.camera_pos.x = cursor_logical.x - (cursor_logical.x - self.camera_pos.x) * zoom_ratio;
                        self.camera_pos.y = cursor_logical.y - (cursor_logical.y - self.camera_pos.y) * zoom_ratio;
                    }

                    return;
                }
                // ----------------------------------

                let source = event.source();

                let horizontal_amount = event
                    .amount(Axis::Horizontal)
                    .unwrap_or_else(|| event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.);
                let vertical_amount = event
                    .amount(Axis::Vertical)
                    .unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.);
                let horizontal_amount_discrete = event.amount_v120(Axis::Horizontal);
                let vertical_amount_discrete = event.amount_v120(Axis::Vertical);

                let mut frame = AxisFrame::new(event.time_msec()).source(source);
                if horizontal_amount != 0.0 {
                    frame = frame.value(Axis::Horizontal, horizontal_amount);
                    if let Some(discrete) = horizontal_amount_discrete {
                        frame = frame.v120(Axis::Horizontal, discrete as i32);
                    }
                }
                if vertical_amount != 0.0 {
                    frame = frame.value(Axis::Vertical, vertical_amount);
                    if let Some(discrete) = vertical_amount_discrete {
                        frame = frame.v120(Axis::Vertical, discrete as i32);
                    }
                }

                if source == AxisSource::Finger {
                    if event.amount(Axis::Horizontal) == Some(0.0) {
                        frame = frame.stop(Axis::Horizontal);
                    }
                    if event.amount(Axis::Vertical) == Some(0.0) {
                        frame = frame.stop(Axis::Vertical);
                    }
                }

                pointer.axis(self, frame);
                pointer.frame(self);
            }

            InputEvent::PointerMotion { .. } => {}

            // Before supporting zoom //
            // InputEvent::PointerMotionAbsolute { event, .. } => {
            //     let output = self.space.outputs().next().unwrap();
            //
            //     let output_geo = self.space.output_geometry(output).unwrap();
            //
            //     let pos = event.position_transformed(output_geo.size) + output_geo.loc.to_f64();
            //
            //     let serial = SERIAL_COUNTER.next_serial();
            //
            //     let pointer = self.seat.get_pointer().unwrap();
            //
            //     let under = self.surface_under(pos);
            //
            //     pointer.motion(
            //         self,
            //         under,
            //         &MotionEvent {
            //             location: pos,
            //             serial,
            //             time: event.time_msec(),
            //         },
            //     );
            //     pointer.frame(self);
            // }
            // InputEvent::PointerMotionAbsolute { event, .. } => {
            //     let output = self.space.outputs().next().unwrap();
            //     let output_geo = self.space.output_geometry(output).unwrap();
            //
            //     // 1. Get the raw physical screen position
            //     let screen_pos = event.position_transformed(output_geo.size) + output_geo.loc.to_f64();
            //
            //     // 2. Apply the Inverse Math to find the Logical World Coordinate
            //     let size = output_geo.size.to_f64();
            //
            //     // Remove screen center offset
            //     let centered_x = screen_pos.x - (size.w / 2.0);
            //     let centered_y = screen_pos.y - (size.h / 2.0);
            //
            //     // Un-scale the zoom
            //     let unscaled_x = centered_x / self.zoom;
            //     let unscaled_y = centered_y / self.zoom;
            //
            //     // Add back the camera position
            //     let logical_pos = smithay::utils::Point::from((
            //         unscaled_x + self.camera_pos.x,
            //         unscaled_y + self.camera_pos.y,
            //     ));
            //
            //     let serial = SERIAL_COUNTER.next_serial();
            //     let pointer = self.seat.get_pointer().unwrap();
            //
            //     // 3. Use the logical position for hit detection
            //     let under = self.surface_under(logical_pos);
            //
            //     // 4. Update Smithay's pointer state with the logical location
            //     pointer.motion(
            //         self,
            //         under,
            //         &MotionEvent {
            //             location: logical_pos, // Pass the translated coordinate
            //             serial,
            //             time: event.time_msec(),
            //         },
            //     );
            //     pointer.frame(self);
            // }
            // InputEvent::PointerButton { event, .. } => {
            //     let pointer = self.seat.get_pointer().unwrap();
            //     let keyboard = self.seat.get_keyboard().unwrap();
            //
            //     let serial = SERIAL_COUNTER.next_serial();
            //
            //     let button = event.button_code();
            //
            //     let button_state = event.state();
            //
            //     if ButtonState::Pressed == button_state && !pointer.is_grabbed() {
            //         if let Some((window, _loc)) = self
            //             .space
            //             .element_under(pointer.current_location())
            //             .map(|(w, l)| (w.clone(), l))
            //         {
            //             self.space.raise_element(&window, true);
            //             keyboard.set_focus(
            //                 self,
            //                 Some(window.toplevel().unwrap().wl_surface().clone()),
            //                 serial,
            //             );
            //             self.space.elements().for_each(|window| {
            //                 window.toplevel().unwrap().send_pending_configure();
            //             });
            //         } else {
            //             self.space.elements().for_each(|window| {
            //                 window.set_activated(false);
            //                 window.toplevel().unwrap().send_pending_configure();
            //             });
            //             keyboard.set_focus(self, Option::<WlSurface>::None, serial);
            //         }
            //     };
            //
            //     pointer.button(
            //         self,
            //         &ButtonEvent {
            //             button,
            //             state: button_state,
            //             serial,
            //             time: event.time_msec(),
            //         },
            //     );
            //     pointer.frame(self);
            // }
            // InputEvent::PointerAxis { event, .. } => {
            //     let source = event.source();
            //
            //     let horizontal_amount = event
            //         .amount(Axis::Horizontal)
            //         .unwrap_or_else(|| event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.);
            //     let vertical_amount = event
            //         .amount(Axis::Vertical)
            //         .unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.);
            //     let horizontal_amount_discrete = event.amount_v120(Axis::Horizontal);
            //     let vertical_amount_discrete = event.amount_v120(Axis::Vertical);
            //
            //     let mut frame = AxisFrame::new(event.time_msec()).source(source);
            //     if horizontal_amount != 0.0 {
            //         frame = frame.value(Axis::Horizontal, horizontal_amount);
            //         if let Some(discrete) = horizontal_amount_discrete {
            //             frame = frame.v120(Axis::Horizontal, discrete as i32);
            //         }
            //     }
            //     if vertical_amount != 0.0 {
            //         frame = frame.value(Axis::Vertical, vertical_amount);
            //         if let Some(discrete) = vertical_amount_discrete {
            //             frame = frame.v120(Axis::Vertical, discrete as i32);
            //         }
            //     }
            //
            //     if source == AxisSource::Finger {
            //         if event.amount(Axis::Horizontal) == Some(0.0) {
            //             frame = frame.stop(Axis::Horizontal);
            //         }
            //         if event.amount(Axis::Vertical) == Some(0.0) {
            //             frame = frame.stop(Axis::Vertical);
            //         }
            //     }
            //
            //     let pointer = self.seat.get_pointer().unwrap();
            //     pointer.axis(self, frame);
            //     pointer.frame(self);
            // }
            _ => {}
        }
    }
}
