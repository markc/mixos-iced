use smithay::backend::renderer::ImportDma;
use smithay::backend::renderer::ImportEgl;
use smithay::backend::renderer::Renderer;
use smithay::backend::renderer::element::AsRenderElements;
use std::time::Duration;
// Newer winit uses 'keyboard' module and 'KeyCode'
// use winit::keyboard::KeyCode;
// use winit::event::ElementState;
// use winit::event::{DeviceEvent};
// use smithay::backend::input::{InputEvent, KeyState};
// Newer winit uses 'keyboard' module and 'KeyCode'
use smithay::backend::renderer::element::{Id, Kind, RenderElement};
use smithay::utils::Point;
use smithay::utils::{Physical, Scale};
use smithay::{
    backend::{
        renderer::{
            damage::OutputDamageTracker, element::surface::WaylandSurfaceRenderElement, gles::GlesRenderer,
        },
        winit::{self as WinitBackend, WinitEvent},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::{Rectangle, Transform},
};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::{
    backend::renderer::{
        element::{Element,  UnderlyingStorage},
        utils::CommitCounter,
    },
    utils::{Buffer, },
};
use smithay::utils::user_data::UserDataMap;
use smithay::backend::renderer::gles::{GlesTexProgram};
use crate::Smallvil;

pub fn init_winit(
    event_loop: &mut EventLoop<Smallvil>,
    state: &mut Smallvil,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = WinitBackend::init::<GlesRenderer>()?;
    // THE FIX: Bind the EGL display to the Wayland display handle.
    // This allows Alacritty to use the 4090's EGL context.

// During your compositor initialization:
//     let custom_program = backend.renderer()
//     .compile_custom_program(
//         // The fragment shader source
//         include_str!("shaders/window.frag"),
//         // A list of the custom uniforms you will pass
//         &["u_zoom", "u_time"]
//     )
//     .expect("Failed to compile custom shader program");
    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };

    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Smithay".into(),
            model: "Winit".into(),
            serial_number: "Unknown".into(),
        },
    );
    let _global = output.create_global::<Smallvil>(&state.display_handle);
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
    output.set_preferred(mode);

    state.space.map_output(&output, (0, 0));

    tracing::info!("INIT EGL");
    tracing::info!("INIT EGL");
    tracing::info!("INIT EGL");
    tracing::info!("INIT EGL");
    tracing::info!("INIT EGL");
    if backend.renderer().bind_wl_display(&state.display_handle).is_ok() {
        tracing::info!("INIT EGL OK");
        println!("EGL Hardware Acceleration bridge initialized for clients.");
    } else {
        tracing::info!("INIT EGL WARN");
        eprintln!("Warning: Clients will not be able to use Hardware Acceleration.");
    }

    // he EGL Bridge (bind_wl_display)
    // What it is: A direct link between your GPU's driver and the Wayland socket.
    //
    // Why we need it: Normally, the compositor's GPU memory is private. This bridge tells the NVIDIA driver: "Hey, if a Wayland app asks for a GPU handle, it's okay to give it one that works with my context." Without this, apps crash with the "Display handle not supported" error.
    //
    // The DMABUF Global
    // What it is: Direct Memory Access Buffer. It’s a way to pass a pointer to a piece of VRAM from the App to the Compositor without copying the actual pixels through the CPU.
    //
    // The "Global" part: In Wayland, a "Global" is an advertisement. Your compositor is now broadcasting: "I speak 4090-memory-language!" Apps see this and switch from slow "Software" mode to fast "Hardware" mode.
    let formats = backend.renderer().dmabuf_formats();
    state.dmabuf_global = Some(
        state
            .dmabuf_state
            .create_global::<Smallvil>(&state.display_handle, formats),
    );

    let mut damage_tracker = OutputDamageTracker::from_output(&output);
    // let timer = Timer::from_duration(Duration::from_secs(5));
    //
    // // 2. Insert it into the loop
    // event_loop.handle().insert_source(timer, |_, _, state| {
    //     println!("5 seconds reached! Zooming out...");
    //     // state.target_zoom = 0.5; // Aim for 50% size (zoomed out)
    //     TimeoutAction::Drop // We only want this to fire once
    // })?;

    event_loop.handle().insert_source(winit, move |event, _, state| {
        match event {
            WinitEvent::Resized { size, .. } => {
                output.change_current_state(
                    Some(Mode {
                        size,
                        refresh: 60_000,
                    }),
                    None,
                    None,
                    None,
                );
            }
            WinitEvent::Input(input_event) => return state.process_input_event(input_event),

            WinitEvent::Redraw => {
                let size = backend.window_size();
                let damage = Rectangle::from_size(size);

                // 1. Calculate the logical dimensions of your camera's view
                let logical_w = size.w as f64 / state.zoom;
                let logical_h = size.h as f64 / state.zoom;

                // Create a bounding box for custom culling (in logical world coordinates)
                let camera_bbox = smithay::utils::Rectangle::from_loc_and_size(
                    (
                        (state.camera_pos.x - logical_w / 2.0) as i32,
                        (state.camera_pos.y - logical_h / 2.0) as i32,
                    ),
                    (logical_w as i32, logical_h as i32),
                );

                // let (renderer, mut framebuffer) = backend.bind().unwrap();
                // let mut elements = Vec::new();
                let mut visible_windows = Vec::new();

                // --- RENDER SCOPE BEGIN ---
                {
                    // 1. Bind the backend. This creates the mutable borrow.
                    let (renderer, mut framebuffer) = backend.bind().unwrap();
                    // let mut elements = Vec::new();
                    let mut elements: Vec<CanvasElement<GlesRenderer>> = Vec::new();

                    // 1. Get the logical cursor position for our hover math
                    let pointer = state.seat.get_pointer().unwrap();
                    let cursor_logical = pointer.current_location();

                    // Define how tall the header should be in logical pixels
                    let header_logical_height = 30.0;

                    // 2. Iterate and generate elements
                    for window in state.space.elements().rev() {
                        let loc = state.space.element_location(window).unwrap_or_default();
                        let window_bbox = state.space.element_bbox(window).unwrap_or_default();

                        if !camera_bbox.overlaps(window_bbox) {
                            continue;
                        }

                        visible_windows.push(window.clone());

                        let bw_logical = 8.0; // Border width
                        let win_w = window_bbox.size.w as f64;
                        let win_h = window_bbox.size.h as f64;

                        // Choose color based on focus
                        // let is_active = window.toplevel().map(|t| t.current_state().activated).unwrap_or(false);
                        let is_active = false;
                        let color = if is_active { [0.0, 0.5, 1.0, 1.0] } else { [0.2, 0.2, 0.2, 1.0] };

                        // Helper to convert World -> Screen coordinates
                        let to_screen = |x: f64, y: f64| {
                            let sx = ((x - state.camera_pos.x) * state.zoom) + (size.w as f64 / 2.0);
                            let sy = ((y - state.camera_pos.y) * state.zoom) + (size.h as f64 / 2.0);
                            (sx as i32, sy as i32)
                        };

                        // 2. Define the 4 border rectangles
                        // Top
                        let top_rect = smithay::utils::Rectangle::from_loc_and_size(
                            to_screen(loc.x as f64 - bw_logical, loc.y as f64 - bw_logical),
                            (((win_w + 2.0 * bw_logical) * state.zoom) as i32, (bw_logical * state.zoom) as i32)
                        );
                        // Bottom
                        let bot_rect = smithay::utils::Rectangle::from_loc_and_size(
                            to_screen(loc.x as f64 - bw_logical, loc.y as f64 + win_h),
                            (((win_w + 2.0 * bw_logical) * state.zoom) as i32, (bw_logical * state.zoom) as i32)
                        );
                        // Left
                        let left_rect = smithay::utils::Rectangle::from_loc_and_size(
                            to_screen(loc.x as f64 - bw_logical, loc.y as f64),
                            ((bw_logical * state.zoom) as i32, (win_h * state.zoom) as i32)
                        );
                        // Right
                        let right_rect = smithay::utils::Rectangle::from_loc_and_size(
                            to_screen(loc.x as f64 + win_w, loc.y as f64),
                            ((bw_logical * state.zoom) as i32, (win_h * state.zoom) as i32)
                        );

                        // 3. Create and push elements
                        for rect in [top_rect, bot_rect, left_rect, right_rect] {
                            elements.push(CanvasElement::SolidBox(SolidColorRenderElement::new(
                                Id::new(),
                                rect,
                                CommitCounter::default(),
                                color,
                                Kind::Unspecified,
                            )));
                        }

                        // [Your world offset and scaling math here]
                        let world_offset_x = loc.x as f64 - state.camera_pos.x;
                        let world_offset_y = loc.y as f64 - state.camera_pos.y;

                        let scaled_x = world_offset_x * state.zoom;
                        let scaled_y = world_offset_y * state.zoom;

                        let screen_x = scaled_x + (size.w as f64 / 2.0);
                        let screen_y = scaled_y + (size.h as f64 / 2.0);

                        let physical_loc = smithay::utils::Point::from((screen_x as i32, screen_y as i32));

                        let window_elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                            window.render_elements(
                                renderer,
                                physical_loc,
                                smithay::utils::Scale::from(state.zoom),
                                1.0,
                            );

                        // 2. Wrap them in the ZoomElement before adding them to the main list
                        let zoomed: Vec<_> = window_elements
                            .into_iter()
                            .map(|e| CanvasElement::Window(ZoomElement {
                                inner: e,
                                zoom: state.zoom,
                            }))
                            .collect();


                        elements.extend(zoomed);


                        // 2. --- HOVER DETECTION ---
                        // Create a logical bounding box for the trigger zone (sitting right on top of the window)
                        let trigger_zone = smithay::utils::Rectangle::from_loc_and_size(
                            (loc.x, loc.y - header_logical_height as i32),
                            (window_bbox.size.w, header_logical_height as i32),
                        );

                        // Check if the logical cursor is inside this trigger zone
                        if trigger_zone.contains(cursor_logical.to_i32_round()) {

                            // 3. --- RENDER THE HEADER ---
                            // Calculate physical placement, just like we did for windows
                            let world_offset_x = loc.x as f64 - state.camera_pos.x;
                            let world_offset_y = (loc.y as f64 - header_logical_height) - state.camera_pos.y;

                            let scaled_x = world_offset_x * state.zoom;
                            let scaled_y = world_offset_y * state.zoom;

                            let screen_x = scaled_x + (size.w as f64 / 2.0);
                            let screen_y = scaled_y + (size.h as f64 / 2.0);

                            // Scale the physical dimensions of the header
                            let physical_width = (window_bbox.size.w as f64 * state.zoom) as i32;
                            let physical_height = (header_logical_height * state.zoom) as i32;

                            let header_rect = smithay::utils::Rectangle::from_loc_and_size(
                                (screen_x as i32, screen_y as i32),
                                (physical_width, physical_height),
                            );

                            // Create a dark gray header bar
                            use smithay::backend::renderer::element::{Id, Kind};
                            use smithay::backend::renderer::utils::CommitCounter;

                            let header_element = SolidColorRenderElement::new(
                                Id::new(),
                                header_rect,
                                CommitCounter::default(),
                                [0.15, 0.15, 0.15, 0.9], // RGBA color
                                Kind::Unspecified,
                            );

                            // Add it to the elements list so it renders!
                            elements.push(CanvasElement::SolidBox(header_element));
                        }
                        // elements.extend(window_elements);
                    }

                    // ... [Inside your render scope, after creating `let mut elements = Vec::new();`] ...



                    // ... [Render the cursor, then call damage_tracker.render_output] ...
                    // Apply the camera math FORWARD to find where the cursor should be on the physical screen
                    let cursor_offset_x = cursor_logical.x - state.camera_pos.x;
                    let cursor_offset_y = cursor_logical.y - state.camera_pos.y;

                    let cursor_scaled_x = cursor_offset_x * state.zoom;
                    let cursor_scaled_y = cursor_offset_y * state.zoom;

                    let cursor_screen_x = cursor_scaled_x + (size.w as f64 / 2.0);
                    let cursor_screen_y = cursor_scaled_y + (size.h as f64 / 2.0);

                    // Make the cursor visually scale with the zoom (change the 20.0 to make it bigger/smaller)
                    let physical_cursor_size = (20.0 * state.zoom) as i32;

                    let cursor_rect = smithay::utils::Rectangle::from_loc_and_size(
                        (cursor_screen_x as i32, cursor_screen_y as i32),
                        (physical_cursor_size, physical_cursor_size),
                    );

                    // Create a SolidColorRenderElement (Red: [1.0, 0.0, 0.0, 1.0])

                    let cursor_element = SolidColorRenderElement::new(
                        Id::new(),                        // Generate a fresh, unique ID
                        cursor_rect,                      // Your calculated geometry
                        CommitCounter::default(),         // Standard commit state
                        [1.0, 0.0, 0.0, 1.0],             // Red color
                        Kind::Unspecified,                // Standard rendering kind for custom elements
                    );


                    // Add it to the very end of the elements list so it renders ON TOP of the windows
                    elements.push(CanvasElement::SolidBox(cursor_element));

                    // 3. Render via Damage Tracker
                    // You correctly added &mut framebuffer here!
                    damage_tracker.render_output(
                        renderer,
                        &mut framebuffer,
                        0,
                        &elements,
                        [0.1, 0.1, 0.1, 1.0]
                    ).unwrap();


                }


                backend.submit(Some(&[damage])).unwrap();

                // 6. Update active windows using our custom visibility list
                for window in visible_windows {
                    window.send_frame(
                        &output,
                        state.start_time.elapsed(),
                        Some(Duration::ZERO),
                        |_, _| Some(output.clone()),
                    );
                }

                state.space.refresh();
                state.popups.cleanup();
                let _ = state.display_handle.flush_clients();
                backend.window().request_redraw();
            }
            // WinitEvent::Redraw => {
            //     let size = backend.window_size();
            //     let damage = Rectangle::from_size(size);
            //
            //     let logical_w = size.w as f64 / state.zoom;
            //     let logical_h = size.h as f64 / state.zoom;
            //
            //     let top_left = smithay::utils::Point::from((
            //         state.camera_pos.x - (logical_w / 2.0),
            //         state.camera_pos.y - (logical_h / 2.0),
            //     ));
            //
            //     // Update the Space's understanding of our output position
            //     state.space.map_output(&output, top_left.to_i32_round());
            //
            //     {
            //         let (renderer, mut framebuffer) = backend.bind().unwrap();
            //         let mut elements = Vec::new();
            //         // 2. CAMERA RENDERING:
            //         // Render only windows that the Space considers "visible" on this output.
            //         // This preserves the Smithay optimizations you requested.
            //         for window in state.space.elements_for_output(&output) {
            //
            //             let loc = state.space.element_location(window).unwrap_or_default();
            //
            //             // 1. Calculate offset from the Camera (the World Center)
            //             let world_offset_x = loc.x as f64 - state.camera_pos.x;
            //             let world_offset_y = loc.y as f64 - state.camera_pos.y;
            //
            //             // 2. Scale that offset
            //             let scaled_x = world_offset_x * state.zoom;
            //             let scaled_y = world_offset_y * state.zoom;
            //
            //             // 3. Move back to Screen Center
            //             let screen_x = scaled_x + (size.w as f64 / 2.0);
            //             let screen_y = scaled_y + (size.h as f64 / 2.0);
            //
            //             let physical_loc = smithay::utils::Point::from((screen_x as i32, screen_y as i32));
            //
            //             let window_elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            //                 window.render_elements(
            //                     renderer,
            //                     physical_loc,
            //                     smithay::utils::Scale::from(state.zoom), // The internal buffer scaling
            //                     // smithay::utils::Scale::from(state.zoom), // The internal buffer scaling
            //                     1.0, // Alpha
            //                 );
            //
            //             elements.extend(window_elements);
            //         }
            //
            //         let no_spaces = None::<&smithay::desktop::space::Space<smithay::desktop::Window>>;
            //         // --- RENDER ---
            //         smithay::desktop::space::render_output::<
            //             _,
            //             WaylandSurfaceRenderElement<GlesRenderer>, // E: The unified Element type
            //             _,
            //             _
            //         >(
            //             &output,
            //             renderer,
            //             &mut framebuffer,
            //             1.0,
            //             0,
            //             no_spaces,
            //             &elements,
            //             &mut damage_tracker,
            //             [0.1, 0.1, 0.1, 1.0],
            //         ).unwrap();
            //     }
            //
            //     backend.submit(Some(&[damage])).unwrap();
            //
            //     // 3. Update active windows
            //     state.space.elements_for_output(&output).for_each(|window| {
            //         window.send_frame(
            //             &output,
            //             state.start_time.elapsed(),
            //             Some(Duration::ZERO),
            //             |_, _| Some(output.clone()),
            //         )
            //     });
            //
            //     state.space.refresh();
            //     state.popups.cleanup();
            //     let _ = state.display_handle.flush_clients();
            //     backend.window().request_redraw();
            // }
            // WinitEvent::Redraw => {
            //     // 1. Progress the animation
            //     state.current_zoom += (state.target_zoom - state.current_zoom) * 0.01;
            //
            //     let size = backend.window_size();
            //     let damage = Rectangle::from_size(size);
            //
            //     {
            //         let (renderer, mut framebuffer) = backend.bind().unwrap();
            //
            //         // --- CAMERA MATH ---
            //         // Find the center of your monitor/window
            //         let center_x = size.w as f64 / 2.0;
            //         let center_y = size.h as f64 / 2.0;
            //         let zoom = state.current_zoom;
            //
            //         let mut elements = Vec::new();
            //
            //         // Iterate through the "blueprint" and apply the camera transformation
            //         for window in state.space.elements() {
            //             // Get the original logical position
            //             let loc = state.space.element_location(window).unwrap_or_default();
            //
            //             // Calculate the new zoomed position relative to the center of the screen
            //             let zoomed_x = center_x + (loc.x as f64 - center_x) * zoom;
            //             let zoomed_y = center_y + (loc.y as f64 - center_y) * zoom;
            //
            //
            //             // Create a logical point (the layout position)
            //             let zoomed_loc: Point<i32, Logical> = (zoomed_x as i32, zoomed_y as i32).into();
            //
            //             // Then convert it to physical pixels for the GPU
            //             let physical_loc = zoomed_loc.to_physical_precise_round(1.0);
            //
            //             let window_elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            //                 window.render_elements(
            //                     renderer,
            //                     physical_loc,
            //                     smithay::utils::Scale::from(zoom),
            //                     1.0,
            //                 );
            //
            //             elements.extend(window_elements);
            //         }
            //         let no_spaces = None::<&smithay::desktop::space::Space<smithay::desktop::Window>>;
            //         // --- RENDER ---
            //         smithay::desktop::space::render_output::<
            //             _,
            //             WaylandSurfaceRenderElement<GlesRenderer>, // E: The unified Element type
            //             _,
            //             _
            //         >(
            //             &output,
            //             renderer,
            //             &mut framebuffer,
            //             1.0,
            //             0,
            //             no_spaces,
            //             &elements, // Use our "Camera" manipulated elements
            //             &mut damage_tracker,
            //             [0.1, 0.1, 0.1, 1.0],
            //         )
            //             .unwrap();
            //     }
            //
            //     backend.submit(Some(&[damage])).unwrap();
            //
            //     state.space.elements().for_each(|window| {
            //         window.send_frame(
            //             &output,
            //             state.start_time.elapsed(),
            //             Some(Duration::ZERO),
            //             |_, _| Some(output.clone()),
            //         )
            //     });
            //
            //     state.space.refresh();
            //     state.popups.cleanup();
            //     let _ = state.display_handle.flush_clients();
            //
            //     // Keep animating until the zoom reaches the target
            //     if (state.target_zoom - state.current_zoom).abs() > 0.001 {
            //         backend.window().request_redraw();
            //     }
            // }
            // WinitEvent::Redraw => {
            //     state.current_zoom += (state.target_zoom - state.current_zoom) * 0.05;
            //
            //     let size = backend.window_size();
            //     let damage = Rectangle::from_size(size);
            //
            //     {
            //         let (renderer, mut framebuffer) = backend.bind().unwrap();
            //
            //         let zoom_scale = 1.0 * state.current_zoom;
            //         smithay::desktop::space::render_output::<
            //             _,
            //             WaylandSurfaceRenderElement<GlesRenderer>,
            //             _,
            //             _,
            //         >(
            //             &output,
            //             renderer,
            //             &mut framebuffer,
            //             1.0,
            //             0,
            //             [&state.space],
            //             &[],
            //             &mut damage_tracker,
            //             [0.1, 0.1, 0.1, 1.0],
            //         )
            //         .unwrap();
            //     }
            //     backend.submit(Some(&[damage])).unwrap();
            //
            //     state.space.elements().for_each(|window| {
            //         window.send_frame(
            //             &output,
            //             state.start_time.elapsed(),
            //             Some(Duration::ZERO),
            //             |_, _| Some(output.clone()),
            //         )
            //     });
            //
            //     state.space.refresh();
            //     state.popups.cleanup();
            //     let _ = state.display_handle.flush_clients();
            //
            //     // Ask for redraw to schedule new frame.
            //     backend.window().request_redraw();
            // }
            WinitEvent::CloseRequested => {
                state.loop_signal.stop();
            }
            _ => (),
        };
    })?;

    Ok(())
}



pub struct ZoomElement<E> {
    pub inner: E,
    pub zoom: f64,
}

impl<E: Element> Element for ZoomElement<E> {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        // THE MAGIC HAPPENS HERE:
        // We ignore the Damage Tracker's scale and force our zoom.
        // This physically stretches the destination mesh.

        self.inner.geometry(Scale::from(self.zoom))
    }
}
impl<R, E> RenderElement<R> for ZoomElement<E>
where
    R: smithay::backend::renderer::Renderer,
    E: RenderElement<R>,
{
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error> {
        // The `dst` passed in here is already scaled by our overridden `geometry()`,
        // so the underlying renderer will automatically stretch the texture to fit!
        self.inner.draw(frame, src, dst, damage, opaque_regions, cache)
    }

    fn underlying_storage(&self, renderer: &mut R) -> Option<UnderlyingStorage> {
        self.inner.underlying_storage(renderer)
    }

    // fn damage_since(&self, _scale: Scale<f64>, commit: Option<CommitCounter>) -> ElementDamage {
    //     // Ensure the damage tracking area is also properly scaled
    //     self.inner.damage_since(Scale::from(self.zoom), commit)
    // }
    //
    // fn alpha(&self) -> f32 {
    //     self.inner.alpha()
    // }

    // fn is_opaque(&self) -> bool {
    //     self.inner.is_opaque()
    // }
}

pub enum CanvasElement<R: Renderer> {
    Window(ZoomElement<WaylandSurfaceRenderElement<R>>),
    // ShadedWindow(ShaderZoomElement),
    SolidBox(SolidColorRenderElement), // Renamed from Cursor
    // Cursor(SolidColorRenderElement),
}

// 4. CANVAS ELEMENT - Base Element Trait
impl<R: Renderer> Element for CanvasElement<R>
where
// This tells Rust to trust that the inner Wayland element is valid
    WaylandSurfaceRenderElement<R>: Element,
{
    fn id(&self) -> &Id {
        match self {
            CanvasElement::Window(w) => w.id(),
            // CanvasElement::ShadedWindow(w) => w.id(),
            CanvasElement::SolidBox(c) => c.id(),
        }
    }

    fn current_commit(&self) -> CommitCounter {
        match self {
            CanvasElement::Window(w) => w.current_commit(),
            // CanvasElement::ShadedWindow(w) => w.current_commit(),
            CanvasElement::SolidBox(c) => c.current_commit(),
        }
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        match self {
            CanvasElement::Window(w) => w.src(),
            // CanvasElement::ShadedWindow(w) => w.src(),
            CanvasElement::SolidBox(c) => c.src(),
        }
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        match self {
            CanvasElement::Window(w) => w.geometry(scale),
            // CanvasElement::ShadedWindow(w) => w.geometry(scale),
            CanvasElement::SolidBox(c) => c.geometry(scale),
        }
    }

    // FIXED: Forwarding the moved Element methods for the Enum too
    // fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> ElementDamage {
    //     match self {
    //         CanvasElement::Window(w) => w.damage_since(scale, commit),
    //         CanvasElement::SolidBox(c) => c.damage_since(scale, commit),
    //     }
    // }

    fn alpha(&self) -> f32 {
        match self {
            CanvasElement::Window(w) => w.alpha(),
            // CanvasElement::ShadedWindow(w) => w.alpha(),
            CanvasElement::SolidBox(c) => c.alpha(),
        }
    }

    // fn is_opaque(&self) -> bool {
    //     match self {
    //         CanvasElement::Window(w) => w.is_opaque(),
    //         CanvasElement::SolidBox(c) => c.is_opaque(),
    //     }
    // }
}

// 5. CANVAS ELEMENT - Render Trait
impl<R: Renderer> RenderElement<R> for CanvasElement<R>
where
// This fixes the compiler error by ensuring R has the necessary Wayland buffer traits
    WaylandSurfaceRenderElement<R>: RenderElement<R>,
{
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error> {
        match self {
            // Force the compiler to use our specific `R` via Fully Qualified Syntax
            CanvasElement::Window(w) => RenderElement::<R>::draw(w, frame, src, dst, damage, opaque_regions, cache),
            // CanvasElement::ShadedWindow(w) => RenderElement::<R>::draw(w, frame, src, dst, damage, opaque_regions, cache),
            CanvasElement::SolidBox(c) => RenderElement::<R>::draw(c, frame, src, dst, damage, opaque_regions, cache),
        }
    }

    fn underlying_storage(&self, renderer: &mut R) -> Option<UnderlyingStorage> {
        match self {
            CanvasElement::Window(w) => w.underlying_storage(renderer),
            CanvasElement::SolidBox(c) => c.underlying_storage(renderer),
        }
    }
}

//
// use smithay::{
//     backend::renderer::{
//         gles::{GlesFrame, GlesTexture, Uniform},
//     },
// };
// pub struct ShaderZoomElement {
//     pub id: Id,
//     pub texture: GlesTexture,
//     pub program: GlesTexProgram,
//     pub zoom: f64,
//     pub time: f32,
//     pub src_rect: Rectangle<f64, Buffer>,
// }
//
// impl smithay::backend::renderer::element::Element for ShaderZoomElement {
//     fn id(&self) -> &smithay::backend::renderer::Id {
//         &self.id
//     }
//
//     fn current_commit(&self) -> smithay::backend::renderer::utils::CommitCounter {
//         smithay::backend::renderer::utils::CommitCounter::default()
//     }
//
//     fn src(&self) -> Rectangle<f64, Buffer> {
//         self.src_rect
//     }
//
//     fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
//         // Apply your magic zoom scale directly to the physical dimensions
//         let mut geo = Rectangle::from_loc_and_size(
//             (0, 0), // Or wherever this element lives
//             self.src_rect.size.to_i32_round(),
//         );
//         geo.size.w = (geo.size.w as f64 * self.zoom) as i32;
//         geo.size.h = (geo.size.h as f64 * self.zoom) as i32;
//         geo
//     }
// }
// impl RenderElement<GlesRenderer> for ShaderZoomElement {
//     fn draw(
//         &self,
//         frame: &mut GlesFrame<'_>,
//         src: Rectangle<f64, Buffer>,
//         dst: Rectangle<i32, Physical>,
//         damage: &[Rectangle<i32, Physical>],
//         opaque_regions: &[Rectangle<i32, Physical>],
//     ) -> Result<(), <GlesRenderer as smithay::backend::renderer::Renderer>::Error> {
//
//         // Prepare the variables you want accessible in GLSL
//         let uniforms = [
//             Uniform::new("u_zoom", self.zoom as f32),
//             Uniform::new("u_time", self.time),
//         ];
//
//         // This is the specific Smithay master API to draw with a custom shader
//         frame.render_texture_from_to(
//             &self.texture,
//             src,
//             dst,
//             damage,
//             opaque_regions,
//             Transform::Normal,
//             1.0, // Alpha
//             Some(&self.program), // Inject your shader here!
//             &uniforms,
//         )
//     }
//
//     fn underlying_storage(
//         &self,
//         _renderer: &mut GlesRenderer,
//     ) -> Option<smithay::backend::renderer::element::UnderlyingStorage> {
//         Some(smithay::backend::renderer::element::UnderlyingStorage::Native(
//             self.texture.clone(),
//         ))
//     }
// }



// use smithay::backend::renderer::gles::{GlesTexProgram, GlesSolidProgram};
//
// // For things with textures (Windows, Background images, Sprite borders)
// let texture_shader: GlesTexProgram = backend.renderer()
//     .compile_custom_texture_shader(
//         include_str!("shaders/actor.frag"),
//         &["u_zoom", "u_time"] // Your custom uniforms
//     )
//     .expect("Failed to compile textured shader");
//
// // For untextured geometry (Solid color borders, simple rects)
// let solid_shader: GlesSolidProgram = backend.renderer()
//     .compile_custom_solid_shader(
//         include_str!("shaders/solid_border.frag"),
//         &["u_time"]
//     )
//     .expect("Failed to compile solid shader");


// use smithay::{
//     backend::renderer::{
//         gles::{GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform},
//         element::{Element, UnderlyingStorage},
//         RenderElement,
//     },
//     utils::{Buffer, Physical, Rectangle, Scale, Transform},
// };
//
// pub struct ActorElement {
//     pub id: smithay::backend::renderer::Id,
//     pub texture: GlesTexture,
//     pub program: GlesTexProgram, // Every actor owns a reference to its shader
//     pub src_rect: Rectangle<f64, Buffer>,
//
//     // Shader state isolated to this specific actor
//     pub zoom: f64,
//     pub time: f32,
//     pub custom_color_tint: [f32; 4],
// }
//
// impl Element for ActorElement {
//     fn id(&self) -> &smithay::backend::renderer::Id { &self.id }
//     fn current_commit(&self) -> smithay::backend::renderer::utils::CommitCounter {
//         smithay::backend::renderer::utils::CommitCounter::default()
//     }
//     fn src(&self) -> Rectangle<f64, Buffer> { self.src_rect }
//     fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
//         // Your magic zoom logic goes here
//         let mut geo = Rectangle::from_loc_and_size((0, 0), self.src_rect.size.to_i32_round());
//         geo.size.w = (geo.size.w as f64 * self.zoom) as i32;
//         geo.size.h = (geo.size.h as f64 * self.zoom) as i32;
//         geo
//     }
// }
//
// impl RenderElement<GlesRenderer> for ActorElement {
//     fn draw(
//         &self,
//         frame: &mut GlesFrame<'_>,
//         src: Rectangle<f64, Buffer>,
//         dst: Rectangle<i32, Physical>,
//         damage: &[Rectangle<i32, Physical>],
//         opaque_regions: &[Rectangle<i32, Physical>],
//     ) -> Result<(), <GlesRenderer as smithay::backend::renderer::Renderer>::Error> {
//
//         let uniforms = [
//             Uniform::new("u_zoom", self.zoom as f32),
//             Uniform::new("u_time", self.time),
//             // Pass any actor-specific data
//         ];
//
//         frame.render_texture_from_to(
//             &self.texture,
//             src,
//             dst,
//             damage,
//             opaque_regions,
//             Transform::Normal,
//             1.0, // Alpha
//             Some(&self.program), // Here is where isolation happens
//             &uniforms,
//         )
//     }
//
//     fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage> {
//         Some(UnderlyingStorage::Native(self.texture.clone()))
//     }
// }


// use smithay::wayland::compositor::with_states;
//
// // For each surface in a window's subsurface tree:
// with_states(surface, |states| {
//     if let Some(buffer) = states.cached_state.get::<smithay::wayland::compositor::BufferAssignment>() {
//         if let Some(wl_buffer) = buffer.buffer() {
//
//             // 1. Import the Wayland Buffer into the GPU
//             if let Ok(texture) = renderer.import_buffer(wl_buffer, Some(states), damage) {
//
//                 // 2. Wrap it in your custom Shader Actor
//                 let actor = ActorElement {
//                     id: smithay::backend::renderer::Id::new(),
//                     texture, // The raw GPU texture of the window
//                     program: texture_shader.clone(),
//                     src_rect: /* ... */,
//                     zoom: current_zoom,
//                     time: current_time,
//                     custom_color_tint: [1.0, 1.0, 1.0, 1.0],
//                 };
//
//                 // 3. Push it to your render queue
//                 render_elements.push(actor);
//             }
//         }
//     }
// });