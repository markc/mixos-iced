use crate::Primitive;
use crate::core::renderer::Quad;
use crate::core::{Background, Color, Gradient, Rectangle, Size, Transformation, Vector};
use crate::graphics::{Image, Text};
use crate::text;

#[derive(Debug)]
pub struct Engine {
    text_pipeline: text::Pipeline,

    #[cfg(test)]
    reference_quad: bool,
    #[cfg(test)]
    quad_path_bounds: Option<tiny_skia::Rect>,

    #[cfg(feature = "image")]
    pub(crate) raster_pipeline: crate::raster::Pipeline,
    #[cfg(feature = "svg")]
    pub(crate) vector_pipeline: crate::vector::Pipeline,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            text_pipeline: text::Pipeline::new(),
            #[cfg(test)]
            reference_quad: false,
            #[cfg(test)]
            quad_path_bounds: None,
            #[cfg(feature = "image")]
            raster_pipeline: crate::raster::Pipeline::new(),
            #[cfg(feature = "svg")]
            vector_pipeline: crate::vector::Pipeline::new(),
        }
    }

    pub fn draw_quad(
        &mut self,
        quad: &Quad,
        background: &Background,
        transformation: Transformation,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: &mut crate::clip::ClipMask<'_>,
        clip_bounds: Rectangle,
    ) {
        let physical_bounds = quad.bounds * transformation;

        if !clip_bounds.intersects(&physical_bounds) {
            return;
        }

        clip_mask.set(clip_bounds);

        let transform = into_transform(transformation);

        #[cfg(test)]
        let optimise_quad = !self.reference_quad;
        #[cfg(not(test))]
        let optimise_quad = true;

        if optimise_quad
            && let Background::Color(color) = background
            && quad.border.width == 0.0
            && <[f32; 4]>::from(quad.border.radius) == [0.0; 4]
            && quad.shadow.color.a == 0.0
            && !physical_bounds.is_within(&clip_bounds)
            && let Some(path) = damage_rectangle(quad.bounds, transform, clip_bounds)
        {
            #[cfg(test)]
            {
                self.quad_path_bounds = Some(path.bounds());
            }
            // A full-window quad under a narrow damage mask otherwise still
            // runs tiny-skia's colour/mask pipeline across the whole window.
            // Restrict geometry at integer pixel boundaries; the original
            // mask continues to decide coverage at fractional clip edges.
            pixels.fill_path(
                &path,
                &tiny_skia::Paint {
                    shader: tiny_skia::Shader::SolidColor(into_color(*color)),
                    anti_alias: true,
                    ..tiny_skia::Paint::default()
                },
                tiny_skia::FillRule::EvenOdd,
                tiny_skia::Transform::identity(),
                Some(clip_mask.mask()),
            );
            return;
        }

        // Make sure the border radius is not larger than the bounds
        let border_width = quad
            .border
            .width
            .min(quad.bounds.width / 2.0)
            .min(quad.bounds.height / 2.0);

        let mut fill_border_radius = <[f32; 4]>::from(quad.border.radius);

        for radius in &mut fill_border_radius {
            *radius = (*radius)
                .min(quad.bounds.width / 2.0)
                .min(quad.bounds.height / 2.0);
        }

        let path = rounded_rectangle(quad.bounds, fill_border_radius);

        let shadow = quad.shadow;

        if shadow.color.a > 0.0 {
            let shadow_bounds = Rectangle {
                x: quad.bounds.x + shadow.offset.x - shadow.blur_radius,
                y: quad.bounds.y + shadow.offset.y - shadow.blur_radius,
                width: quad.bounds.width + shadow.blur_radius * 2.0,
                height: quad.bounds.height + shadow.blur_radius * 2.0,
            } * transformation;

            let radii = fill_border_radius
                .into_iter()
                .map(|radius| radius * transformation.scale_factor())
                .collect::<Vec<_>>();
            let (x, y, width, height) = (
                shadow_bounds.x as u32,
                shadow_bounds.y as u32,
                shadow_bounds.width as u32,
                shadow_bounds.height as u32,
            );
            let half_width = physical_bounds.width / 2.0;
            let half_height = physical_bounds.height / 2.0;

            let colors = (y..y + height)
                .flat_map(|y| (x..x + width).map(move |x| (x as f32, y as f32)))
                .filter_map(|(x, y)| {
                    tiny_skia::Size::from_wh(half_width, half_height).map(|size| {
                        let shadow_distance = rounded_box_sdf(
                            Vector::new(
                                x - physical_bounds.position().x
                                    - (shadow.offset.x * transformation.scale_factor())
                                    - half_width,
                                y - physical_bounds.position().y
                                    - (shadow.offset.y * transformation.scale_factor())
                                    - half_height,
                            ),
                            size,
                            &radii,
                        )
                        .max(0.0);
                        let shadow_alpha = 1.0
                            - smoothstep(
                                -shadow.blur_radius * transformation.scale_factor(),
                                shadow.blur_radius * transformation.scale_factor(),
                                shadow_distance,
                            );

                        let mut color = into_color(shadow.color);
                        color.apply_opacity(shadow_alpha);

                        color.to_color_u8().premultiply()
                    })
                })
                .collect();

            if let Some(pixmap) = tiny_skia::IntSize::from_wh(width, height)
                .and_then(|size| tiny_skia::Pixmap::from_vec(bytemuck::cast_vec(colors), size))
            {
                pixels.draw_pixmap(
                    x as i32,
                    y as i32,
                    pixmap.as_ref(),
                    &tiny_skia::PixmapPaint::default(),
                    tiny_skia::Transform::default(),
                    Some(clip_mask.mask()),
                );
            }
        }

        let clip_mask = (!physical_bounds.is_within(&clip_bounds)).then_some(clip_mask.mask());

        pixels.fill_path(
            &path,
            &tiny_skia::Paint {
                shader: match background {
                    Background::Color(color) => tiny_skia::Shader::SolidColor(into_color(*color)),
                    Background::Gradient(Gradient::Linear(linear)) => {
                        let (start, end) = quad.bounds.chord(linear.angle);

                        let stops: Vec<tiny_skia::GradientStop> = linear
                            .stops
                            .into_iter()
                            .flatten()
                            .map(|stop| {
                                tiny_skia::GradientStop::new(
                                    stop.offset,
                                    tiny_skia::Color::from_rgba(
                                        stop.color.b,
                                        stop.color.g,
                                        stop.color.r,
                                        stop.color.a,
                                    )
                                    .expect("Create color"),
                                )
                            })
                            .collect();

                        tiny_skia::LinearGradient::new(
                            tiny_skia::Point {
                                x: start.x,
                                y: start.y,
                            },
                            tiny_skia::Point { x: end.x, y: end.y },
                            if stops.is_empty() {
                                vec![tiny_skia::GradientStop::new(0.0, tiny_skia::Color::BLACK)]
                            } else {
                                stops
                            },
                            tiny_skia::SpreadMode::Pad,
                            tiny_skia::Transform::identity(),
                        )
                        .expect("Create linear gradient")
                    }
                },
                anti_alias: true,
                ..tiny_skia::Paint::default()
            },
            tiny_skia::FillRule::EvenOdd,
            transform,
            clip_mask,
        );

        if border_width > 0.0 {
            // Border path is offset by half the border width
            let border_bounds = Rectangle {
                x: quad.bounds.x + border_width / 2.0,
                y: quad.bounds.y + border_width / 2.0,
                width: quad.bounds.width - border_width,
                height: quad.bounds.height - border_width,
            };

            // Make sure the border radius is correct
            let mut border_radius = <[f32; 4]>::from(quad.border.radius);
            let mut is_simple_border = true;

            for radius in &mut border_radius {
                *radius = if *radius == 0.0 {
                    // Path should handle this fine
                    0.0
                } else if *radius > border_width / 2.0 {
                    *radius - border_width / 2.0
                } else {
                    is_simple_border = false;
                    0.0
                }
                .min(border_bounds.width / 2.0)
                .min(border_bounds.height / 2.0);
            }

            // Stroking a path works well in this case
            if is_simple_border {
                let border_path = rounded_rectangle(border_bounds, border_radius);

                pixels.stroke_path(
                    &border_path,
                    &tiny_skia::Paint {
                        shader: tiny_skia::Shader::SolidColor(into_color(quad.border.color)),
                        anti_alias: true,
                        ..tiny_skia::Paint::default()
                    },
                    &tiny_skia::Stroke {
                        width: border_width,
                        ..tiny_skia::Stroke::default()
                    },
                    transform,
                    clip_mask,
                );
            } else {
                // Draw corners that have too small border radii as having no border radius,
                // but mask them with the rounded rectangle with the correct border radius.
                let mut temp_pixmap =
                    tiny_skia::Pixmap::new(quad.bounds.width as u32, quad.bounds.height as u32)
                        .unwrap();

                let mut quad_mask =
                    tiny_skia::Mask::new(quad.bounds.width as u32, quad.bounds.height as u32)
                        .unwrap();

                let zero_bounds = Rectangle {
                    x: 0.0,
                    y: 0.0,
                    width: quad.bounds.width,
                    height: quad.bounds.height,
                };
                let path = rounded_rectangle(zero_bounds, fill_border_radius);

                quad_mask.fill_path(&path, tiny_skia::FillRule::EvenOdd, true, transform);
                let path_bounds = Rectangle {
                    x: border_width / 2.0,
                    y: border_width / 2.0,
                    width: quad.bounds.width - border_width,
                    height: quad.bounds.height - border_width,
                };

                let border_radius_path = rounded_rectangle(path_bounds, border_radius);

                temp_pixmap.stroke_path(
                    &border_radius_path,
                    &tiny_skia::Paint {
                        shader: tiny_skia::Shader::SolidColor(into_color(quad.border.color)),
                        anti_alias: true,
                        ..tiny_skia::Paint::default()
                    },
                    &tiny_skia::Stroke {
                        width: border_width,
                        ..tiny_skia::Stroke::default()
                    },
                    transform,
                    Some(&quad_mask),
                );

                pixels.draw_pixmap(
                    quad.bounds.x as i32,
                    quad.bounds.y as i32,
                    temp_pixmap.as_ref(),
                    &tiny_skia::PixmapPaint::default(),
                    transform,
                    clip_mask,
                );
            }
        }
    }

    pub fn draw_text(
        &mut self,
        text: &Text,
        transformation: Transformation,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: &mut crate::clip::ClipMask<'_>,
        clip_bounds: Rectangle,
    ) {
        match text {
            Text::Paragraph {
                paragraph,
                position,
                color,
                clip_bounds: local_clip_bounds,
                transformation: local_transformation,
            } => {
                let transformation = transformation * *local_transformation;
                let Some(clip_bounds) =
                    clip_bounds.intersection(&(*local_clip_bounds * transformation))
                else {
                    return;
                };

                let physical_bounds =
                    Rectangle::new(*position, paragraph.min_bounds) * transformation;

                if !clip_bounds.intersects(&physical_bounds) {
                    return;
                }

                let clip_mask = match physical_bounds.is_within(&clip_bounds) {
                    true => None,
                    false => {
                        clip_mask.set(clip_bounds);
                        Some(clip_mask.mask())
                    }
                };

                self.text_pipeline.draw_paragraph(
                    paragraph,
                    *position,
                    *color,
                    pixels,
                    clip_mask,
                    clip_bounds,
                    transformation,
                );
            }
            Text::Editor {
                editor,
                position,
                color,
                clip_bounds: local_clip_bounds,
                transformation: local_transformation,
            } => {
                let transformation = transformation * *local_transformation;

                let Some(clip_bounds) =
                    clip_bounds.intersection(&(*local_clip_bounds * transformation))
                else {
                    return;
                };

                clip_mask.set(clip_bounds);

                self.text_pipeline.draw_editor(
                    editor,
                    *position,
                    *color,
                    pixels,
                    Some(clip_mask.mask()),
                    clip_bounds,
                    transformation,
                );
            }
            Text::Cached {
                content,
                bounds,
                color,
                size,
                line_height,
                font,
                align_x,
                align_y,
                shaping,
                wrapping,
                ellipsis,
                clip_bounds: local_clip_bounds,
            } => {
                let physical_bounds = *local_clip_bounds * transformation;

                if !clip_bounds.intersects(&physical_bounds) {
                    return;
                }

                let clip_mask = match physical_bounds.is_within(&clip_bounds) {
                    true => None,
                    false => {
                        clip_mask.set(clip_bounds);
                        Some(clip_mask.mask())
                    }
                };

                self.text_pipeline.draw_cached(
                    content,
                    *bounds,
                    *color,
                    *size,
                    *line_height,
                    *font,
                    *align_x,
                    *align_y,
                    *shaping,
                    *wrapping,
                    *ellipsis,
                    pixels,
                    clip_mask,
                    clip_bounds,
                    transformation,
                );
            }
            Text::Raw {
                raw,
                transformation: local_transformation,
            } => {
                let Some(buffer) = raw.buffer.upgrade() else {
                    return;
                };

                let transformation = transformation * *local_transformation;
                let (width, height) = buffer.size();

                let physical_bounds = Rectangle::new(
                    raw.position,
                    Size::new(
                        width.unwrap_or(clip_bounds.width),
                        height.unwrap_or(clip_bounds.height),
                    ),
                ) * transformation;

                if !clip_bounds.intersects(&physical_bounds) {
                    return;
                }

                let clip_mask = if physical_bounds.is_within(&clip_bounds) {
                    None
                } else {
                    clip_mask.set(clip_bounds);
                    Some(clip_mask.mask())
                };

                self.text_pipeline.draw_raw(
                    &buffer,
                    raw.position,
                    raw.color,
                    pixels,
                    clip_mask,
                    clip_bounds,
                    transformation,
                );
            }
        }
    }

    pub fn draw_primitive(
        &mut self,
        primitive: &Primitive,
        transformation: Transformation,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: &mut crate::clip::ClipMask<'_>,
        clip_bounds: Rectangle,
    ) {
        match primitive {
            Primitive::Fill { path, paint, rule } => {
                let physical_bounds = {
                    let bounds = path.bounds();

                    Rectangle {
                        x: bounds.x(),
                        y: bounds.y(),
                        width: bounds.width(),
                        height: bounds.height(),
                    } * transformation
                };

                if !clip_bounds.intersects(&physical_bounds) {
                    return;
                }

                let clip_mask = if physical_bounds.is_within(&clip_bounds) {
                    None
                } else {
                    clip_mask.set(clip_bounds);
                    Some(clip_mask.mask())
                };

                pixels.fill_path(
                    path,
                    paint,
                    *rule,
                    into_transform(transformation),
                    clip_mask,
                );
            }
            Primitive::Stroke {
                path,
                paint,
                stroke,
            } => {
                let physical_bounds = {
                    let bounds = path.bounds();

                    Rectangle {
                        x: bounds.x() - stroke.width / 2.0,
                        y: bounds.y() - stroke.width / 2.0,
                        width: bounds.width() + stroke.width,
                        height: bounds.height() + stroke.width,
                    } * transformation
                };

                if !clip_bounds.intersects(&physical_bounds) {
                    return;
                }

                let clip_mask = if physical_bounds.is_within(&clip_bounds) {
                    None
                } else {
                    clip_mask.set(clip_bounds);
                    Some(clip_mask.mask())
                };

                pixels.stroke_path(
                    path,
                    paint,
                    stroke,
                    into_transform(transformation),
                    clip_mask,
                );
            }
        }
    }

    pub fn draw_layer_image(
        &mut self,
        image: &crate::layer::Image,
        transformation: Transformation,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: &mut crate::clip::ClipMask<'_>,
        clip_bounds: Rectangle,
    ) {
        match image {
            crate::layer::Image::Standard(image) => {
                self.draw_image(image, transformation, pixels, clip_mask, clip_bounds)
            }
            #[cfg(feature = "image")]
            crate::layer::Image::Grid {
                grid,
                bounds,
                clip_bounds: grid_clip,
            } => {
                let Some(clip) = clip_bounds.intersection(&(*grid_clip * transformation)) else {
                    return;
                };
                let physical = *bounds * transformation;
                if !clip.intersects(&physical) {
                    return;
                }
                if grid.copy(*bounds, pixels, into_transform(transformation), clip) {
                    return;
                }
                // Native copy reads only the rectangle, never this mask. The
                // fallback needs the intersection including the widget clip.
                let masked = !physical.is_within(&clip);
                if masked {
                    clip_mask.set(clip);
                }
                grid.draw_fallback(
                    *bounds,
                    pixels,
                    into_transform(transformation),
                    masked.then_some(clip_mask.mask()),
                );
                if masked {
                    clip_mask.set(clip_bounds);
                }
            }
        }
    }

    pub fn draw_image(
        &mut self,
        image: &Image,
        _transformation: Transformation,
        _pixels: &mut tiny_skia::PixmapMut<'_>,
        _clip_mask: &mut crate::clip::ClipMask<'_>,
        _clip_bounds: Rectangle,
    ) {
        match image {
            #[cfg(feature = "image")]
            Image::Raster {
                image,
                bounds,
                clip_bounds: local_clip_bounds,
            } => {
                let physical_bounds = *local_clip_bounds * _transformation;

                let Some(clip_bounds) = physical_bounds.intersection(&_clip_bounds) else {
                    return;
                };

                // TODO: Border radius
                _clip_mask.set(clip_bounds);

                let center = bounds.center();
                let radians = f32::from(image.rotation);

                let transform = into_transform(_transformation).pre_rotate_at(
                    radians.to_degrees(),
                    center.x,
                    center.y,
                );

                self.raster_pipeline.draw(
                    &image.handle,
                    image.filter_method,
                    *bounds,
                    image.opacity,
                    _pixels,
                    transform,
                    Some(_clip_mask.mask()),
                    clip_bounds,
                );
            }
            #[cfg(feature = "svg")]
            Image::Vector { svg, bounds, .. } => {
                let physical_bounds = *bounds * _transformation;

                if !_clip_bounds.intersects(&physical_bounds) {
                    return;
                }

                let clip_mask = if physical_bounds.is_within(&_clip_bounds) {
                    None
                } else {
                    _clip_mask.set(_clip_bounds);
                    Some(_clip_mask.mask())
                };

                let center = physical_bounds.center();
                let radians = f32::from(svg.rotation);

                let transform = into_transform(_transformation).post_rotate_at(
                    radians.to_degrees(),
                    center.x,
                    center.y,
                );

                self.vector_pipeline.draw(
                    &svg.handle,
                    svg.color,
                    *bounds,
                    svg.opacity,
                    _pixels,
                    transform,
                    clip_mask,
                );
            }
            #[cfg(not(feature = "image"))]
            Image::Raster { .. } => {
                log::warn!("Unsupported primitive in `iced_tiny_skia`: {image:?}",);
            }
            #[cfg(not(feature = "svg"))]
            Image::Vector { .. } => {
                log::warn!("Unsupported primitive in `iced_tiny_skia`: {image:?}",);
            }
        }
    }

    pub fn trim(&mut self) {
        self.text_pipeline.trim_cache();

        #[cfg(feature = "image")]
        self.raster_pipeline.trim_cache();

        #[cfg(feature = "svg")]
        self.vector_pipeline.trim_cache();
    }
}

pub fn into_color(color: Color) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba(color.b, color.g, color.r, color.a)
        .expect("Convert color from iced to tiny_skia")
}

fn into_transform(transformation: Transformation) -> tiny_skia::Transform {
    let translation = transformation.translation();

    tiny_skia::Transform {
        sx: transformation.scale_factor(),
        kx: 0.0,
        ky: 0.0,
        sy: transformation.scale_factor(),
        tx: translation.x,
        ty: translation.y,
    }
}

/// Preserve the transformed quad's fractional edges and cut only outside
/// the clip's conservative integer pixel bounds. Using the original path's
/// transform avoids changing rounding via `(x * scale) + (width * scale)`.
fn damage_rectangle(
    bounds: Rectangle,
    transform: tiny_skia::Transform,
    clip: Rectangle,
) -> Option<tiny_skia::Path> {
    if !transform.sx.is_finite() || transform.sx <= 0.0 {
        return None;
    }
    let path = tiny_skia::PathBuilder::from_rect(tiny_skia::Rect::from_xywh(
        bounds.x,
        bounds.y,
        bounds.width,
        bounds.height,
    )?)
    .transform(transform)?;
    let original = path.bounds();
    let clipped = tiny_skia::Rect::from_ltrb(
        original.left().max(clip.x.floor()),
        original.top().max(clip.y.floor()),
        original.right().min((clip.x + clip.width).ceil()),
        original.bottom().min((clip.y + clip.height).ceil()),
    )?;
    Some(tiny_skia::PathBuilder::from_rect(clipped))
}

#[cfg(test)]
mod quad_tests {
    use super::*;
    use crate::clip::ClipMask;
    use crate::core::{Border, Shadow};

    fn draw(
        engine: &mut Engine,
        target: &mut tiny_skia::Pixmap,
        quad: &Quad,
        background: Background,
        transform: Transformation,
        clips: &[Rectangle],
    ) {
        let mut storage = tiny_skia::Mask::new(target.width(), target.height()).unwrap();
        let mut mask = ClipMask::new(&mut storage);
        for &clip in clips {
            engine.draw_quad(
                quad,
                &background,
                transform,
                &mut target.as_mut(),
                &mut mask,
                clip,
            );
        }
    }

    #[test]
    fn solid_quad_damage_matches_full_path_pixel_oracle() {
        let bounds = [
            Rectangle {
                x: -2.75,
                y: -1.49,
                width: 70.25,
                height: 54.5,
            },
            Rectangle {
                x: 3.125,
                y: 6.75,
                width: 35.125,
                height: 21.49,
            },
            Rectangle {
                x: 27.499,
                y: -20.25,
                width: 20.01,
                height: 100.75,
            },
            Rectangle {
                x: 67.875,
                y: 58.125,
                width: 50.5,
                height: 40.75,
            },
            Rectangle {
                x: -100.5,
                y: 2.125,
                width: 102.75,
                height: 40.75,
            },
        ];
        let clips = [
            Rectangle {
                x: -5.5,
                y: -4.75,
                width: 18.49,
                height: 26.25,
            },
            Rectangle {
                x: 17.25,
                y: 10.5,
                width: 2.5,
                height: 19.75,
            },
            Rectangle {
                x: 3.49,
                y: 6.51,
                width: 0.25,
                height: 0.5,
            },
            Rectangle {
                x: 27.51,
                y: 8.49,
                width: 20.25,
                height: 4.75,
            },
            Rectangle {
                x: 93.25,
                y: 68.25,
                width: 15.5,
                height: 20.0,
            },
            Rectangle {
                x: 180.0,
                y: 140.0,
                width: 4.25,
                height: 2.5,
            },
            Rectangle {
                x: 0.0,
                y: 0.0,
                width: 96.0,
                height: 72.0,
            },
        ];
        for scale in [1.0, 1.25, 1.5, 2.5] {
            for translation in [(0.0, 0.0), (-9.125, 3.49), (7.51, -2.25)] {
                let transform = Transformation::translate(translation.0, translation.1)
                    * Transformation::scale(scale);
                for bounds in bounds {
                    for alpha in [0.0, 0.17, 0.5, 1.0] {
                        let quad = Quad {
                            bounds,
                            ..Quad::default()
                        };
                        let background =
                            Background::Color(Color::from_rgba(0.71, 0.23, 0.49, alpha));
                        for clip in clips {
                            let mut expected = tiny_skia::Pixmap::new(96, 72).unwrap();
                            expected.fill(tiny_skia::Color::from_rgba8(30, 60, 90, 150));
                            let mut actual = expected.clone();
                            let mut original = Engine::new();
                            original.reference_quad = true;
                            draw(
                                &mut original,
                                &mut expected,
                                &quad,
                                background,
                                transform,
                                &[clip],
                            );
                            draw(
                                &mut Engine::new(),
                                &mut actual,
                                &quad,
                                background,
                                transform,
                                &[clip],
                            );
                            assert_eq!(
                                actual.data(),
                                expected.data(),
                                "bounds={bounds:?} scale={scale} translation={translation:?} alpha={alpha} clip={clip:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn narrow_damage_bounds_actual_quad_raster_work() {
        let quad = Quad {
            bounds: Rectangle {
                x: -0.25,
                y: -0.5,
                width: 2250.5,
                height: 1250.75,
            },
            ..Quad::default()
        };
        let clip = Rectangle {
            x: 500.25,
            y: 300.5,
            width: 2.5,
            height: 27.25,
        };
        let mut target = tiny_skia::Pixmap::new(2250, 1250).unwrap();
        let mut engine = Engine::new();
        draw(
            &mut engine,
            &mut target,
            &quad,
            Color::BLACK.into(),
            Transformation::IDENTITY,
            &[clip],
        );
        let path = engine.quad_path_bounds.expect("production fast path ran");
        assert_eq!(
            (path.left(), path.top(), path.right(), path.bottom()),
            (500.0, 300.0, 503.0, 328.0)
        );
        assert_eq!(path.width() * path.height(), 84.0);
    }

    #[test]
    fn decorated_and_gradient_quads_keep_original_path() {
        let base = Quad {
            bounds: Rectangle {
                x: 10.25,
                y: 10.5,
                width: 50.0,
                height: 40.0,
            },
            ..Quad::default()
        };
        let clip = Rectangle {
            x: 15.25,
            y: 15.5,
            width: 2.5,
            height: 19.75,
        };
        let gradient = crate::core::gradient::Linear::new(0.37)
            .add_stop(0.0, Color::BLACK)
            .add_stop(1.0, Color::WHITE);
        for (quad, background) in [
            (
                Quad {
                    border: Border::default().rounded(5.0),
                    ..base
                },
                Color::BLACK.into(),
            ),
            (
                Quad {
                    border: Border::default().width(1.25).color(Color::WHITE),
                    ..base
                },
                Color::BLACK.into(),
            ),
            (
                Quad {
                    shadow: Shadow {
                        color: Color::from_rgba(0.0, 0.0, 0.0, 0.5),
                        blur_radius: 2.0,
                        ..Shadow::default()
                    },
                    ..base
                },
                Color::BLACK.into(),
            ),
            (base, Background::Gradient(gradient.into())),
        ] {
            let mut expected = tiny_skia::Pixmap::new(96, 72).unwrap();
            let mut actual = expected.clone();
            let mut original = Engine::new();
            original.reference_quad = true;
            let mut engine = Engine::new();
            draw(
                &mut original,
                &mut expected,
                &quad,
                background,
                Transformation::scale(1.25),
                &[clip],
            );
            draw(
                &mut engine,
                &mut actual,
                &quad,
                background,
                Transformation::scale(1.25),
                &[clip],
            );
            assert_eq!(actual.data(), expected.data());
            assert!(engine.quad_path_bounds.is_none());
        }
    }

    #[test]
    #[ignore = "manual release benchmark; run on a build node"]
    fn narrow_quad_damage_benchmark() {
        use std::hint::black_box;
        use std::time::Instant;
        let quad = Quad {
            bounds: Rectangle {
                x: 0.0,
                y: 0.0,
                width: 2250.0,
                height: 1250.0,
            },
            ..Quad::default()
        };
        let clip = Rectangle {
            x: 500.25,
            y: 300.5,
            width: 2.5,
            height: 27.25,
        };
        let mut expected = tiny_skia::Pixmap::new(2250, 1250).unwrap();
        let mut actual = expected.clone();
        let mut timings = Vec::new();
        for (target, reference) in [(&mut expected, true), (&mut actual, false)] {
            let mut engine = Engine::new();
            engine.reference_quad = reference;
            let mut storage = tiny_skia::Mask::new(2250, 1250).unwrap();
            let mut mask = ClipMask::new(&mut storage);
            mask.set(clip); // Exclude first-mask initialisation from timing.
            let started = Instant::now();
            for _ in 0..1000 {
                engine.draw_quad(
                    &quad,
                    &Color::BLACK.into(),
                    Transformation::IDENTITY,
                    &mut target.as_mut(),
                    &mut mask,
                    black_box(clip),
                );
                let _ = black_box(target.data());
            }
            timings.push(started.elapsed());
        }
        assert_eq!(actual.data(), expected.data());
        eprintln!(
            "narrow quad: 2250x1250 full path vs 3x28 damage, 1000 frames; original={:?} bounded={:?} ratio={:.2}x",
            timings[0],
            timings[1],
            timings[0].as_secs_f64() / timings[1].as_secs_f64()
        );
    }
}

fn rounded_rectangle(bounds: Rectangle, border_radius: [f32; 4]) -> tiny_skia::Path {
    let [top_left, top_right, bottom_right, bottom_left] = border_radius;

    if top_left == 0.0 && top_right == 0.0 && bottom_right == 0.0 && bottom_left == 0.0 {
        return tiny_skia::PathBuilder::from_rect(
            tiny_skia::Rect::from_xywh(bounds.x, bounds.y, bounds.width, bounds.height)
                .expect("Build quad rectangle"),
        );
    }

    if top_left == top_right
        && top_left == bottom_right
        && top_left == bottom_left
        && top_left == bounds.width / 2.0
        && top_left == bounds.height / 2.0
    {
        return tiny_skia::PathBuilder::from_circle(
            bounds.x + bounds.width / 2.0,
            bounds.y + bounds.height / 2.0,
            top_left,
        )
        .expect("Build circle path");
    }

    let mut builder = tiny_skia::PathBuilder::new();

    builder.move_to(bounds.x + top_left, bounds.y);
    builder.line_to(bounds.x + bounds.width - top_right, bounds.y);

    if top_right > 0.0 {
        arc_to(
            &mut builder,
            bounds.x + bounds.width - top_right,
            bounds.y,
            bounds.x + bounds.width,
            bounds.y + top_right,
            top_right,
        );
    }

    maybe_line_to(
        &mut builder,
        bounds.x + bounds.width,
        bounds.y + bounds.height - bottom_right,
    );

    if bottom_right > 0.0 {
        arc_to(
            &mut builder,
            bounds.x + bounds.width,
            bounds.y + bounds.height - bottom_right,
            bounds.x + bounds.width - bottom_right,
            bounds.y + bounds.height,
            bottom_right,
        );
    }

    maybe_line_to(
        &mut builder,
        bounds.x + bottom_left,
        bounds.y + bounds.height,
    );

    if bottom_left > 0.0 {
        arc_to(
            &mut builder,
            bounds.x + bottom_left,
            bounds.y + bounds.height,
            bounds.x,
            bounds.y + bounds.height - bottom_left,
            bottom_left,
        );
    }

    maybe_line_to(&mut builder, bounds.x, bounds.y + top_left);

    if top_left > 0.0 {
        arc_to(
            &mut builder,
            bounds.x,
            bounds.y + top_left,
            bounds.x + top_left,
            bounds.y,
            top_left,
        );
    }

    builder.finish().expect("Build rounded rectangle path")
}

fn maybe_line_to(path: &mut tiny_skia::PathBuilder, x: f32, y: f32) {
    if path.last_point() != Some(tiny_skia::Point { x, y }) {
        path.line_to(x, y);
    }
}

fn arc_to(
    path: &mut tiny_skia::PathBuilder,
    x_from: f32,
    y_from: f32,
    x_to: f32,
    y_to: f32,
    radius: f32,
) {
    let svg_arc = kurbo::SvgArc {
        from: kurbo::Point::new(f64::from(x_from), f64::from(y_from)),
        to: kurbo::Point::new(f64::from(x_to), f64::from(y_to)),
        radii: kurbo::Vec2::new(f64::from(radius), f64::from(radius)),
        x_rotation: 0.0,
        large_arc: false,
        sweep: true,
    };

    match kurbo::Arc::from_svg_arc(&svg_arc) {
        Some(arc) => {
            arc.to_cubic_beziers(0.1, |p1, p2, p| {
                path.cubic_to(
                    p1.x as f32,
                    p1.y as f32,
                    p2.x as f32,
                    p2.y as f32,
                    p.x as f32,
                    p.y as f32,
                );
            });
        }
        None => {
            path.line_to(x_to, y_to);
        }
    }
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let x = ((x - a) / (b - a)).clamp(0.0, 1.0);

    x * x * (3.0 - 2.0 * x)
}

fn rounded_box_sdf(to_center: Vector, size: tiny_skia::Size, radii: &[f32]) -> f32 {
    let radius = match (to_center.x > 0.0, to_center.y > 0.0) {
        (true, true) => radii[2],
        (true, false) => radii[1],
        (false, true) => radii[3],
        (false, false) => radii[0],
    };

    let x = (to_center.x.abs() - size.width() + radius).max(0.0);
    let y = (to_center.y.abs() - size.height() + radius).max(0.0);

    (x.powf(2.0) + y.powf(2.0)).sqrt() - radius
}

#[cfg(test)]
pub fn adjust_clip_mask(clip_mask: &mut tiny_skia::Mask, bounds: Rectangle) {
    clip_mask.clear();

    let path = tiny_skia::PathBuilder::from_rect(
        tiny_skia::Rect::from_xywh(bounds.x, bounds.y, bounds.width, bounds.height)
            .expect("Create clip rectangle"),
    );

    clip_mask.fill_path(
        &path,
        tiny_skia::FillRule::EvenOdd,
        false,
        tiny_skia::Transform::default(),
    );
}
