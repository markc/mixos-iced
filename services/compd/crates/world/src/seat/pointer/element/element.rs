use std::sync::Mutex;

use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{AsRenderElements, Kind, RenderElement};
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer, Texture};
use smithay::input::pointer::CursorImageStatus;
use smithay::render_elements;
use smithay::utils::{Physical, Point, Scale};
use std::time::{Duration, Instant};
use crate::seat::pointer::texture::pointer_load::{Cursor, CursorThemeCache};

use std::sync::Arc;

render_elements! {
    pub PointerRenderElement<R> where R: ImportAll + ImportMem;
    Surface=WaylandSurfaceRenderElement<R>,
    Memory=MemoryRenderBufferRenderElement<R>,
}

pub struct PointerElement {
    pub theme: Arc<CursorThemeCache>,
    pub status: CursorImageStatus,
    /// Tracks the currently selected named cursor and when it was selected,
    /// for animation frame timing. Reset whenever the effective name changes.
    anim_state: Mutex<AnimState>,
}

struct AnimState {
    current_name: Option<String>,
    started_at: Instant,
}

impl PointerElement {
    pub fn new(theme: Arc<CursorThemeCache>) -> Self {
        Self {
            theme,
            status: CursorImageStatus::default_named(),
            anim_state: Mutex::new(AnimState {
                current_name: None,
                started_at: Instant::now(),
            }),
        }
    }

    /// Returns the active frame index for an animated cursor, given the
    /// elapsed time since this name was first selected.
    fn current_frame_index(&self, name: &str, cursor: &Cursor) -> usize {
        if cursor.frames.len() <= 1 {
            return 0;
        }

        let mut state = self.anim_state.lock().unwrap();
        let now = Instant::now();

        // Reset the animation clock whenever the effective cursor changes,
        // so a freshly-selected `wait` starts at frame 0 rather than mid-spin.
        if state.current_name.as_deref() != Some(name) {
            state.current_name = Some(name.to_string());
            state.started_at = now;
        }

        let total: Duration = cursor.frames.iter().map(|f| f.delay).sum();
        if total.is_zero() {
            return 0;
        }

        let elapsed = now.duration_since(state.started_at);
        let mut t = Duration::from_nanos((elapsed.as_nanos() % total.as_nanos()) as u64);

        for (i, frame) in cursor.frames.iter().enumerate() {
            if t < frame.delay {
                return i;
            }
            t -= frame.delay;
        }
        cursor.frames.len() - 1
    }

    pub fn get_current_hotspot(&self) -> Point<i32, Physical> {
        match &self.status {
            CursorImageStatus::Surface(surface) => {
                smithay::wayland::compositor::with_states(surface, |states| {
                    let attrs = states
                        .data_map
                        .get::<Mutex<smithay::input::pointer::CursorImageAttributes>>()
                        .unwrap()
                        .lock()
                        .unwrap();
                    Point::from((attrs.hotspot.x, attrs.hotspot.y))
                })
            }
            CursorImageStatus::Named(name) => self
                .theme
                .get(name.name())
                .map(|c| c.hotspot)
                .unwrap_or_else(|| (0, 0).into()),
            CursorImageStatus::Hidden => (0, 0).into(),
        }
    }
}

impl<T, R> AsRenderElements<R> for PointerElement
where
    T: Texture + Clone + Send + 'static,
    R: Renderer<TextureId = T> + ImportAll + ImportMem,
{
    type RenderElement = PointerRenderElement<R>;

    fn render_elements<E>(
        &self,
        renderer: &mut R,
        location: Point<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
    ) -> Vec<E>
    where
        E: From<PointerRenderElement<R>>,
    {
        match &self.status {
            CursorImageStatus::Hidden => vec![],

            CursorImageStatus::Surface(surface) => {
                let elements: Vec<PointerRenderElement<R>> =
                    smithay::backend::renderer::element::surface::render_elements_from_surface_tree(
                        renderer,
                        surface,
                        location,
                        scale,
                        alpha,
                        Kind::Cursor,
                    );
                elements.into_iter().map(E::from).collect()
            }

            CursorImageStatus::Named(name) => {
                // Loaded at the output scale and drawn
                // at its logical size, the base cursor's: the whole image is the
                // source, and `size` is its pixels × base / nominal, which the
                // output scale then maps back to about the image's own pixels.
                let Some(cursor) = self.theme.get_scaled(name.name(), scale.x) else {
                    return vec![];
                };

                let frame_idx = self.current_frame_index(name.name(), &cursor);
                let frame = &cursor.frames[frame_idx];
                let ratio = f64::from(self.theme.base_size()) / f64::from(cursor.nominal.max(1));
                let pixels = (f64::from(cursor.size.w), f64::from(cursor.size.h));
                let src = smithay::utils::Rectangle::<f64, smithay::utils::Logical>::from_size(pixels.into());
                let logical = smithay::utils::Size::<i32, smithay::utils::Logical>::from((
                    (pixels.0 * ratio).round().max(1.0) as i32,
                    (pixels.1 * ratio).round().max(1.0) as i32,
                ));

                let Ok(elem) = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    location.to_f64(),
                    &frame.buffer,
                    None,
                    Some(src),
                    Some(logical),
                    Kind::Cursor,
                ) else {
                    return vec![];
                };

                vec![E::from(PointerRenderElement::from(elem))]
            }
        }
    }
}
