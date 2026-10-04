//! Opt-in KMS evidence: COMPD_KMS_DIAG_DIR, three frames per activation/output.

use scenegraph::scene::element::element::SceneElement;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{ExportMem, Texture};
use smithay::utils::Rectangle;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

struct Diagnostics {
    dir: PathBuf,
    frames: HashMap<usize, u8>,
    ids: HashMap<Id, usize>,
    logged: HashSet<(usize, Id)>,
}

thread_local! {
    // Independent of the dump opt-in and retained across VT activations: an
    // unchanged count must not be logged again just because the session resumed.
    static ICED_COUNTS: RefCell<HashMap<String, usize>> = RefCell::new(HashMap::new());
    static DIAG: RefCell<Option<Diagnostics>> = RefCell::new(
        std::env::var_os("COMPD_KMS_DIAG_DIR").map(|dir| Diagnostics {
            dir: dir.into(),
            frames: HashMap::new(),
            ids: HashMap::new(),
            logged: HashSet::new(),
        })
    );
}

/// The GLES lowerer emits these two variants for compositor-owned iced. The
/// native GlesElementWrapper is only a newtype around a reference to this enum;
/// it does not turn iced into Texture/TextureCropped (those hold imported dmabufs).
pub(super) fn iced_texture(element: &SceneElement<GlesRenderer>) -> Option<&GlesTexture> {
    match element {
        SceneElement::Surface(iced) => Some(&iced.texture),
        SceneElement::SurfaceCropped(iced) => Some(&iced.element().texture),
        _ => None,
    }
}

/// Count the final list handed to KMS, including zero, before plane assignment.
/// Use the connector name in the log and keep independent state for each output.
pub(super) fn iced_elements(output: &str, elements: &[SceneElement<GlesRenderer>]) {
    let count = elements
        .iter()
        .filter(|element| iced_texture(element).is_some())
        .count();
    ICED_COUNTS.with(|counts| {
        let mut counts = counts.borrow_mut();
        if counts.get(output).copied() == Some(count) {
            return;
        }
        counts.insert(output.to_owned(), count);
        info!("KMS_DIAG iced_elements output={output} count={count}");
    });
}

pub(super) fn enabled() -> bool {
    DIAG.with(|diag| diag.borrow().is_some())
}

/// The initial active session starts at frame 1 too. A VT return re-arms dumps
/// and one metadata line per surface/output, so assignments describe this visit.
pub(crate) fn activated() {
    DIAG.with(|diag| {
        if let Some(diag) = diag.borrow_mut().as_mut() {
            diag.frames.clear();
            diag.logged.clear();
        }
    });
}

pub(super) struct DumpFrame {
    pub dir: PathBuf,
    pub number: u8,
}

pub(super) fn frame(output: usize) -> Option<DumpFrame> {
    DIAG.with(|diag| {
        let mut diag = diag.borrow_mut();
        let diag = diag.as_mut()?;
        let frame = diag.frames.entry(output).or_default();
        if *frame == 3 {
            return None;
        }
        *frame += 1;
        Some(DumpFrame {
            // Keep the requested names at the root for the primary output.
            // Secondary outputs must not overwrite its evidence.
            dir: if output == 0 {
                diag.dir.clone()
            } else {
                diag.dir.join(format!("output-{output}"))
            },
            number: *frame,
        })
    })
}

/// Smithay's Id is opaque. Give it a short filename id and log the exact Id
/// alongside it; the same surface keeps its dump id across outputs/activations.
pub(super) fn surface(id: &Id, output: usize) -> (usize, bool) {
    DIAG.with(|diag| {
        let mut diag = diag.borrow_mut();
        let diag = diag.as_mut().unwrap(); // caller gates on enabled()
        let next = diag.ids.len() + 1;
        let dump_id = *diag.ids.entry(id.clone()).or_insert(next);
        (dump_id, diag.logged.insert((output, id.clone())))
    })
}

pub(super) fn iced(
    renderer: &mut GlesRenderer,
    texture: &GlesTexture,
    id: &Id,
    dump_id: usize,
    frame: &DumpFrame,
) {
    let path = frame
        .dir
        .join(format!("iced-{dump_id}-{}.ppm", frame.number));
    match write_texture(renderer, texture, &path, false) {
        Ok((alpha, _)) => info!(
            "KMS_DIAG texture id={id:?} dump_id={dump_id} frame={} nonzero_alpha={alpha}/{} file={}",
            frame.number,
            texture.width() as usize * texture.height() as usize,
            path.display(),
        ),
        Err(err) => warn!(
            "KMS_DIAG texture id={id:?} frame={} file={} error={err}",
            frame.number,
            path.display()
        ),
    }
}

pub(super) fn primary(renderer: &mut GlesRenderer, texture: &GlesTexture, frame: &DumpFrame) {
    let path = frame.dir.join(format!("primary-{}.ppm", frame.number));
    match write_texture(renderer, texture, &path, true) {
        Ok((_, colours)) => info!(
            "KMS_DIAG primary frame={} size={}x{} distinct_colours={colours} file={}",
            frame.number,
            texture.width(),
            texture.height(),
            path.display(),
        ),
        Err(err) => warn!(
            "KMS_DIAG primary frame={} file={} error={err}",
            frame.number,
            path.display()
        ),
    }
}

/// copy_texture binds the texture to an FBO and issues glReadPixels into a PBO;
/// map_texture waits for that readback. ABGR8888 is byte-order RGBA on our hosts.
/// These offscreen targets use top-down GL storage, as does the capture path;
/// only an explicitly y-inverted source needs its rows reversed for the PPM.
fn write_texture(
    renderer: &mut GlesRenderer,
    texture: &GlesTexture,
    path: &Path,
    distinct: bool,
) -> Result<(usize, usize), String> {
    let size = texture.size();
    if size.w <= 0 || size.h <= 0 {
        return Err(format!("invalid texture size: {size:?}"));
    }
    let mapping = renderer
        .copy_texture(texture, Rectangle::from_size(size), Fourcc::Abgr8888)
        .map_err(|err| format!("copy_texture: {err}"))?;
    let bytes = renderer
        .map_texture(&mapping)
        .map_err(|err| format!("map_texture: {err}"))?;
    let row = size.w as usize * 4;
    let len = row * size.h as usize;
    if bytes.len() < len {
        return Err(format!("readback length: {} < {len}", bytes.len()));
    }
    let mut rgb = Vec::with_capacity(len / 4 * 3);
    let mut alpha = 0;
    let mut colours = HashSet::new();
    for y in 0..size.h as usize {
        let src = if texture.is_y_inverted() {
            size.h as usize - 1 - y
        } else {
            y
        };
        for pixel in bytes[src * row..(src + 1) * row].chunks_exact(4) {
            alpha += usize::from(pixel[3] != 0);
            rgb.extend_from_slice(&pixel[..3]);
            if distinct {
                colours.insert([pixel[0], pixel[1], pixel[2]]);
            }
        }
    }
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
        write!(file, "P6\n{} {}\n255\n", size.w, size.h)?;
        file.write_all(&rgb)?;
        file.flush()
    };
    write().map_err(|err| format!("write PPM: {err}"))?;
    Ok((alpha, colours.len()))
}
