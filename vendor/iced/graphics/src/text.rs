//! Draw text.
pub mod cache;
pub mod editor;
pub mod paragraph;

pub use cache::Cache;
pub use editor::Editor;
pub use paragraph::Paragraph;

pub use cosmic_text;

use crate::core::alignment;
use crate::core::font::{self, Font};
use crate::core::text::{Alignment, Ellipsis, Shaping, Wrapping};
use crate::core::{Color, Pixels, Point, Rectangle, Size, Transformation};

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock, RwLock, Weak};

/// A text primitive.
#[derive(Debug, Clone, PartialEq)]
pub enum Text {
    /// A paragraph.
    #[allow(missing_docs)]
    Paragraph {
        paragraph: paragraph::Weak,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
        transformation: Transformation,
    },
    /// An editor.
    #[allow(missing_docs)]
    Editor {
        editor: editor::Weak,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
        transformation: Transformation,
    },
    /// Some cached text.
    Cached {
        /// The contents of the text.
        content: String,
        /// The bounds of the text.
        bounds: Rectangle,
        /// The color of the text.
        color: Color,
        /// The size of the text in logical pixels.
        size: Pixels,
        /// The line height of the text.
        line_height: Pixels,
        /// The font of the text.
        font: Font,
        /// The horizontal alignment of the text.
        align_x: Alignment,
        /// The vertical alignment of the text.
        align_y: alignment::Vertical,
        /// The shaping strategy of the text.
        shaping: Shaping,
        /// The wrapping strategy of the text.
        wrapping: Wrapping,
        /// The ellipsis strategy of the text.
        ellipsis: Ellipsis,
        /// The clip bounds of the text.
        clip_bounds: Rectangle,
    },
    /// Some raw text.
    #[allow(missing_docs)]
    Raw {
        raw: Raw,
        transformation: Transformation,
    },
}

impl Text {
    /// Returns the visible bounds of the [`Text`].
    pub fn visible_bounds(&self) -> Option<Rectangle> {
        match self {
            Text::Paragraph {
                position,
                paragraph,
                clip_bounds,
                transformation,
                ..
            } => Rectangle::new(*position, paragraph.min_bounds)
                .intersection(clip_bounds)
                .map(|bounds| bounds * *transformation),
            Text::Editor {
                editor,
                position,
                clip_bounds,
                transformation,
                ..
            } => Rectangle::new(*position, editor.bounds)
                .intersection(clip_bounds)
                .map(|bounds| bounds * *transformation),
            Text::Cached {
                bounds,
                clip_bounds,
                ..
            } => bounds.intersection(clip_bounds),
            Text::Raw { raw, .. } => Some(raw.clip_bounds),
        }
    }
}

/// The regular variant of the [Fira Sans] font.
///
/// It is loaded as part of the default fonts when the `fira-sans`
/// feature is enabled.
///
/// [Fira Sans]: https://mozilla.github.io/Fira/
#[cfg(feature = "fira-sans")]
pub const FIRA_SANS_REGULAR: &[u8] = include_bytes!("../fonts/FiraSans-Regular.ttf").as_slice();

/// Returns the global [`FontSystem`].
pub fn font_system() -> &'static RwLock<FontSystem> {
    static FONT_SYSTEM: OnceLock<RwLock<FontSystem>> = OnceLock::new();

    FONT_SYSTEM.get_or_init(|| {
        #[allow(unused_mut)]
        let mut raw = cosmic_text::FontSystem::new_with_fonts([
            cosmic_text::fontdb::Source::Binary(Arc::new(
                include_bytes!("../fonts/Iced-Icons.ttf").as_slice(),
            )),
            #[cfg(feature = "fira-sans")]
            cosmic_text::fontdb::Source::Binary(Arc::new(
                include_bytes!("../fonts/FiraSans-Regular.ttf").as_slice(),
            )),
        ]);

        #[cfg(feature = "fira-sans")]
        raw.db_mut().set_sans_serif_family("Fira Sans");

        #[cfg(target_os = "macos")]
        {
            #[cfg(not(feature = "fira-sans"))]
            raw.db_mut().set_sans_serif_family(".SF NS");
            raw.db_mut().set_serif_family("Times New Roman");
            raw.db_mut().set_monospace_family("Menlo");
        }

        #[cfg(target_os = "windows")]
        {
            #[cfg(not(feature = "fira-sans"))]
            raw.db_mut().set_sans_serif_family("Segoe UI");
            raw.db_mut().set_serif_family("Times New Roman");
            raw.db_mut().set_monospace_family("Consolas");
        }

        RwLock::new(FontSystem {
            raw,
            loaded_fonts: HashSet::new(),
            version: Version::default(),
        })
    })
}

/// A set of system fonts.
pub struct FontSystem {
    raw: cosmic_text::FontSystem,
    loaded_fonts: HashSet<usize>,
    version: Version,
}

impl FontSystem {
    /// Returns the raw [`cosmic_text::FontSystem`].
    pub fn raw(&mut self) -> &mut cosmic_text::FontSystem {
        &mut self.raw
    }

    /// Loads a font from its bytes.
    pub fn load_font(&mut self, bytes: Cow<'static, [u8]>) {
        if let Cow::Borrowed(bytes) = bytes {
            let address = bytes.as_ptr() as usize;

            if !self.loaded_fonts.insert(address) {
                return;
            }
        }

        let loaded = self
            .raw
            .db_mut()
            .load_font_source(cosmic_text::fontdb::Source::Binary(Arc::new(
                bytes.into_owned(),
            )));

        // Rebuild the derived indexes and clear the match cache after the
        // successful mutation; unparsable bytes leave the system untouched.
        if loaded.is_some() {
            self.raw.refresh_database();

            self.version = Version(self.version.0 + 1);
        }
    }

    /// Registers new font faces and pinned alias policies in one atomic
    /// cosmic-text transaction, and bumps the [`FontSystem::version`] exactly
    /// once when faces or policies were actually added. A transaction that is
    /// empty or identical to an already installed policy is a no-op and does
    /// not change the version.
    pub fn register_fonts(
        &mut self,
        registration: cosmic_text::FontRegistration,
    ) -> Result<cosmic_text::FontRegistrationResult, cosmic_text::FontRegistrationError> {
        // The version must be able to advance before the cosmic transaction
        // commits; the hypothetical overflow is caught up front rather than
        // reported as an error after the mutation.
        assert!(
            self.version.0 != u32::MAX,
            "iced_graphics font system version overflow"
        );

        let result = self.raw.register_fonts(registration)?;

        if !result.added_faces.is_empty() || result.policies_added > 0 {
            self.version = Version(self.version.0 + 1);
        }

        Ok(result)
    }

    /// Returns an iterator over the family names of all font faces
    /// in the font database.
    pub fn families(&self) -> impl Iterator<Item = &str> {
        self.raw
            .db()
            .faces()
            .filter_map(|face| face.families.first())
            .map(|(name, _)| name.as_str())
    }

    /// Returns the current [`Version`] of the [`FontSystem`].
    ///
    /// Loading a font will increase the version of a [`FontSystem`].
    pub fn version(&self) -> Version {
        self.version
    }
}

/// A version number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Version(u32);

/// A weak reference to a [`cosmic_text::Buffer`] that can be drawn.
#[derive(Debug, Clone)]
pub struct Raw {
    /// A weak reference to a [`cosmic_text::Buffer`].
    pub buffer: Weak<cosmic_text::Buffer>,
    /// The position of the text.
    pub position: Point,
    /// The color of the text.
    pub color: Color,
    /// The clip bounds of the text.
    pub clip_bounds: Rectangle,
}

impl PartialEq for Raw {
    fn eq(&self, _other: &Self) -> bool {
        // TODO: There is no proper way to compare raw buffers
        // For now, no two instances of `Raw` text will be equal.
        // This should be fine, but could trigger unnecessary redraws
        // in the future.
        false
    }
}

/// Measures the dimensions of the given [`cosmic_text::Buffer`].
pub fn measure(buffer: &cosmic_text::Buffer) -> (Size, bool) {
    let (width, height, has_rtl) =
        buffer
            .layout_runs()
            .fold((0.0, 0.0, false), |(width, height, has_rtl), run| {
                (
                    run.line_w.max(width),
                    height + run.line_height,
                    has_rtl || run.rtl,
                )
            });

    (Size::new(width, height), has_rtl)
}

/// Aligns the given [`cosmic_text::Buffer`] with the given [`Alignment`]
/// and returns its minimum [`Size`].
pub fn align(
    buffer: &mut cosmic_text::Buffer,
    font_system: &mut cosmic_text::FontSystem,
    alignment: Alignment,
) -> Size {
    let (min_bounds, has_rtl) = measure(buffer);
    let mut needs_relayout = has_rtl;

    if let Some(align) = to_align(alignment) {
        let has_multiple_lines = buffer.lines.len() > 1
            || buffer
                .lines
                .first()
                .is_some_and(|line| line.layout_opt().is_some_and(|layout| layout.len() > 1));

        if has_multiple_lines {
            for line in &mut buffer.lines {
                let _ = line.set_align(Some(align));
            }

            needs_relayout = true;
        } else if let Some(line) = buffer.lines.first_mut() {
            needs_relayout |= line.set_align(None);
        }
    }

    // TODO: Avoid relayout with some changes to `cosmic-text` (?)
    if needs_relayout {
        log::trace!("Relayouting paragraph...");

        buffer.set_size(Some(min_bounds.width), Some(min_bounds.height));
        buffer.shape_until_scroll(font_system, false);
    }

    min_bounds
}

/// Returns the attributes of the given [`Font`].
pub fn to_attributes(font: Font) -> cosmic_text::Attrs<'static> {
    cosmic_text::Attrs::new()
        .family(to_family(font.family))
        .weight(to_weight(font.weight))
        .stretch(to_stretch(font.stretch))
        .style(to_style(font.style))
}

fn to_family(family: font::Family) -> cosmic_text::Family<'static> {
    match family {
        font::Family::Name(name) => cosmic_text::Family::Name(name),
        font::Family::SansSerif => cosmic_text::Family::SansSerif,
        font::Family::Serif => cosmic_text::Family::Serif,
        font::Family::Cursive => cosmic_text::Family::Cursive,
        font::Family::Fantasy => cosmic_text::Family::Fantasy,
        font::Family::Monospace => cosmic_text::Family::Monospace,
    }
}

fn to_weight(weight: font::Weight) -> cosmic_text::Weight {
    cosmic_text::Weight(weight.value())
}

fn to_stretch(stretch: font::Stretch) -> cosmic_text::Stretch {
    match stretch {
        font::Stretch::UltraCondensed => cosmic_text::Stretch::UltraCondensed,
        font::Stretch::ExtraCondensed => cosmic_text::Stretch::ExtraCondensed,
        font::Stretch::Condensed => cosmic_text::Stretch::Condensed,
        font::Stretch::SemiCondensed => cosmic_text::Stretch::SemiCondensed,
        font::Stretch::Normal => cosmic_text::Stretch::Normal,
        font::Stretch::SemiExpanded => cosmic_text::Stretch::SemiExpanded,
        font::Stretch::Expanded => cosmic_text::Stretch::Expanded,
        font::Stretch::ExtraExpanded => cosmic_text::Stretch::ExtraExpanded,
        font::Stretch::UltraExpanded => cosmic_text::Stretch::UltraExpanded,
    }
}

fn to_style(style: font::Style) -> cosmic_text::Style {
    match style {
        font::Style::Normal => cosmic_text::Style::Normal,
        font::Style::Italic => cosmic_text::Style::Italic,
        font::Style::Oblique => cosmic_text::Style::Oblique,
    }
}

fn to_align(alignment: Alignment) -> Option<cosmic_text::Align> {
    match alignment {
        Alignment::Default => None,
        Alignment::Left => Some(cosmic_text::Align::Left),
        Alignment::Center => Some(cosmic_text::Align::Center),
        Alignment::Right => Some(cosmic_text::Align::Right),
        Alignment::Justified => Some(cosmic_text::Align::Justified),
    }
}

/// Converts some [`Shaping`] strategy to a [`cosmic_text::Shaping`] strategy.
pub fn to_shaping(shaping: Shaping, text: &str) -> cosmic_text::Shaping {
    match shaping {
        Shaping::Auto => {
            if text.is_ascii() {
                cosmic_text::Shaping::Basic
            } else {
                cosmic_text::Shaping::Advanced
            }
        }
        Shaping::Basic => cosmic_text::Shaping::Basic,
        Shaping::Advanced => cosmic_text::Shaping::Advanced,
    }
}

/// Converts some [`Wrapping`] strategy to a [`cosmic_text::Wrap`] strategy.
pub fn to_wrap(wrapping: Wrapping) -> cosmic_text::Wrap {
    match wrapping {
        Wrapping::None => cosmic_text::Wrap::None,
        Wrapping::Word => cosmic_text::Wrap::Word,
        Wrapping::Glyph => cosmic_text::Wrap::Glyph,
        Wrapping::WordOrGlyph => cosmic_text::Wrap::WordOrGlyph,
    }
}

/// Converts some [`Ellipsis`] strategy to a [`cosmic_text::Ellipsize`] strategy.
pub fn to_ellipsize(ellipsis: Ellipsis, max_height: f32) -> cosmic_text::Ellipsize {
    let limit = cosmic_text::EllipsizeHeightLimit::Height(max_height);

    match ellipsis {
        Ellipsis::None => cosmic_text::Ellipsize::None,
        Ellipsis::Start => cosmic_text::Ellipsize::Start(limit),
        Ellipsis::Middle => cosmic_text::Ellipsize::Middle(limit),
        Ellipsis::End => cosmic_text::Ellipsize::End(limit),
    }
}

/// Converts some [`Color`] to a [`cosmic_text::Color`].
pub fn to_color(color: Color) -> cosmic_text::Color {
    let [r, g, b, a] = color.into_rgba8();

    cosmic_text::Color::rgba(r, g, b, a)
}

/// Returns the ideal hint factor given the size and scale factor of some text.
pub fn hint_factor(_size: Pixels, _scale_factor: Option<f32>) -> Option<f32> {
    // TODO: Fix hinting in `cosmic-text`
    // const MAX_HINTING_SIZE: f32 = 18.0;

    // let hint_factor = scale_factor?;

    // if size.0 * hint_factor < MAX_HINTING_SIZE {
    //     Some(hint_factor)
    // } else {
    //     None
    // }

    None // Disable all text hinting for now
}

/// A text renderer coupled to `iced_graphics`.
pub trait Renderer {
    /// Draws the given [`Raw`] text.
    fn fill_raw(&mut self, raw: Raw);
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic_text::{FontRegistration, PinnedFaceRef, PinnedFontPolicy};

    /// Parse the bundled icon font into a binary `FaceInfo`; the scratch
    /// database supplies the metadata and the caller's `Arc` owns the bytes.
    fn binary_face(bytes: &'static [u8]) -> cosmic_text::fontdb::FaceInfo {
        let mut scratch = cosmic_text::fontdb::Database::new();
        let _ = scratch.load_font_source(cosmic_text::fontdb::Source::Binary(Arc::new(
            bytes.to_vec(),
        )));
        let mut face = scratch.faces().next().expect("font has a face").clone();
        face.source = cosmic_text::fontdb::Source::Binary(Arc::new(bytes.to_vec()));
        face
    }

    fn system() -> FontSystem {
        FontSystem {
            raw: cosmic_text::FontSystem::new_with_fonts([]),
            loaded_fonts: HashSet::new(),
            version: Version::default(),
        }
    }

    #[test]
    fn register_bumps_version_once_and_duplicate_policy_does_not() {
        let mut system = system();
        let bytes: &'static [u8] = include_bytes!("../fonts/Iced-Icons.ttf");

        let face = binary_face(bytes);
        let weight = face.weight;
        let alias = "iced-registration-test-alias".to_owned();
        let first = system
            .register_fonts(FontRegistration {
                faces: vec![face],
                policies: vec![PinnedFontPolicy {
                    alias: alias.clone(),
                    groups: vec![vec![PinnedFaceRef::Added(0)]],
                    weight,
                }],
            })
            .expect("first registration succeeds");

        assert_eq!(first.added_faces.len(), 1);
        assert_eq!(first.policies_added, 1);
        assert_ne!(system.version(), Version::default());

        // Re-registering the identical resolved policy with no new faces is a
        // no-op: no retained growth, no version change.
        let once = system.version();
        let added_id = first.added_faces[0];
        let noop = system
            .register_fonts(FontRegistration {
                faces: Vec::new(),
                policies: vec![PinnedFontPolicy {
                    alias: alias.clone(),
                    groups: vec![vec![PinnedFaceRef::Existing(added_id)]],
                    weight,
                }],
            })
            .expect("duplicate policy is a no-op");
        assert!(noop.added_faces.is_empty());
        assert_eq!(noop.policies_added, 0);
        assert_eq!(system.version(), once);

        // A conflicting rebind fails and leaves the version unchanged. Pick a
        // weight the static icon face cannot provide (it has no `wght` axis),
        // so the attempt fails even if its default weight were 900.
        let rebound = if weight.0 == 900 {
            cosmic_text::fontdb::Weight(100)
        } else {
            cosmic_text::fontdb::Weight::BLACK
        };
        let conflict = system.register_fonts(FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias,
                groups: vec![vec![PinnedFaceRef::Existing(added_id)]],
                weight: rebound,
            }],
        });
        assert!(conflict.is_err(), "rebinding an alias must fail");
        assert_eq!(system.version(), once);
    }

    #[test]
    fn one_transaction_with_many_faces_bumps_version_once() {
        let mut system = system();
        let bytes: &'static [u8] = include_bytes!("../fonts/Iced-Icons.ttf");

        let face_a = binary_face(bytes);
        let face_b = binary_face(bytes);
        let face_c = binary_face(bytes);
        let weight = face_a.weight;

        let before = system.version();
        let result = system
            .register_fonts(FontRegistration {
                faces: vec![face_a, face_b, face_c],
                policies: vec![
                    PinnedFontPolicy {
                        alias: "iced-registration-test-alias-2".to_owned(),
                        groups: vec![vec![PinnedFaceRef::Added(0)]],
                        weight,
                    },
                    PinnedFontPolicy {
                        alias: "iced-registration-test-alias-3".to_owned(),
                        groups: vec![vec![PinnedFaceRef::Added(1)]],
                        weight,
                    },
                ],
            })
            .expect("multi-face transaction succeeds");

        assert_eq!(result.added_faces.len(), 3);
        assert_eq!(result.policies_added, 2);
        assert_ne!(system.version(), before, "one bump for the transaction");

        // The single bump is observable: an identical no-op transaction
        // leaves the version where the transaction left it.
        let after = system.version();
        let noop = system
            .register_fonts(FontRegistration {
                faces: Vec::new(),
                policies: vec![PinnedFontPolicy {
                    alias: "iced-registration-test-alias-2".to_owned(),
                    groups: vec![vec![PinnedFaceRef::Existing(result.added_faces[0])]],
                    weight,
                }],
            })
            .expect("no-op succeeds");
        assert!(noop.added_faces.is_empty());
        assert_eq!(noop.policies_added, 0);
        assert_eq!(system.version(), after);
    }

    #[test]
    fn retained_paragraph_stays_pinned_across_registration_version() {
        use crate::core::text::{Difference, LineHeight, Paragraph as _, Text as CoreText};
        use cosmic_text::SwashCache;

        let bytes: &'static [u8] = include_bytes!("../fonts/Iced-Icons.ttf");
        let alias: &'static str =
            Box::leak("iced-paragraph-stability-alias".to_owned().into_boxed_str());

        // Register the pinned face in the shared font system and remember its
        // ID; the registration bumps the global version.
        let (face_id, weight) = {
            let mut system = font_system().write().expect("font system");
            let face = binary_face(bytes);
            let weight = face.weight;
            let result = system
                .register_fonts(FontRegistration {
                    faces: vec![face],
                    policies: vec![PinnedFontPolicy {
                        alias: alias.to_owned(),
                        groups: vec![vec![PinnedFaceRef::Added(0)]],
                        weight,
                    }],
                })
                .expect("registration");
            (result.added_faces[0], weight)
        };

        let font = Font {
            family: font::Family::Name(alias),
            ..Font::DEFAULT
        };
        let text = CoreText {
            content: "Hello world",
            bounds: Size::new(300.0, 100.0),
            size: Pixels(16.0),
            line_height: LineHeight::Absolute(Pixels(20.0)),
            font,
            align_x: Alignment::Left,
            align_y: alignment::Vertical::Top,
            shaping: Shaping::Advanced,
            wrapping: Wrapping::None,
            ellipsis: Ellipsis::None,
            hint_factor: None,
        };

        // The paragraph is retained across the second registration below.
        let paragraph = Paragraph::with_text(text);

        let glyphs = |paragraph: &Paragraph| {
            paragraph
                .buffer()
                .layout_runs()
                .flat_map(|run| run.glyphs.iter())
                .map(|g| (g.font_id, g.font_weight, g.glyph_id, g.w))
                .collect::<Vec<_>>()
        };
        let before = glyphs(&paragraph);
        assert!(!before.is_empty());
        assert!(
            before
                .iter()
                .all(|(id, _, glyph_id, _)| *id == face_id && *glyph_id != 0)
        );

        // A second registration bumps the version again; the retained
        // paragraph must report a Shape difference through iced's
        // paragraph-comparison path, which is what triggers a re-shape.
        {
            let mut system = font_system().write().expect("font system");
            system
                .register_fonts(FontRegistration {
                    faces: vec![binary_face(bytes)],
                    policies: vec![PinnedFontPolicy {
                        alias: "iced-paragraph-stability-alias-2".to_owned(),
                        groups: vec![vec![PinnedFaceRef::Added(0)]],
                        weight,
                    }],
                })
                .expect("registration");
        }
        let probe = CoreText {
            content: (),
            bounds: text.bounds,
            size: text.size,
            line_height: text.line_height,
            font: text.font,
            align_x: text.align_x,
            align_y: text.align_y,
            shaping: text.shaping,
            wrapping: text.wrapping,
            ellipsis: text.ellipsis,
            hint_factor: text.hint_factor,
        };
        assert_eq!(
            paragraph.compare(probe),
            Difference::Shape,
            "a version bump must mark the retained paragraph for re-shaping"
        );

        // Re-shaping the same text keeps the original face and metrics.
        let reshaped = Paragraph::with_text(text);
        let after = glyphs(&reshaped);
        assert_eq!(before, after, "reshaped paragraph must stay pinned");

        // Raster evidence: the reshaped paragraph still rasterises from the
        // pinned face.
        let cache_key = reshaped
            .buffer()
            .layout_runs()
            .flat_map(|run| run.glyphs.iter())
            .next()
            .expect("glyph")
            .physical((0.0, 0.0), 1.0)
            .cache_key;
        let image = {
            let mut system = font_system().write().expect("font system");
            let mut cache = SwashCache::new();
            cache
                .get_image(system.raw(), cache_key)
                .as_ref()
                .map(|image| image.data.clone())
        };
        assert!(
            image.is_some(),
            "retained paragraph must still rasterise from its pinned face"
        );
    }
}
