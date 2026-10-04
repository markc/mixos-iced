//! Preference-driven primary selection with a fallback chain.
//! Consumes ranked plain values (Law 3): exclusion first, then preference
//! order, then the provided default heuristic result, then first survivor.

use render_gles::preference::gpu::rank::rank::GpuRank;
use std::path::PathBuf;

pub fn select(
    candidates: &[PathBuf],
    default_heuristic: Option<&PathBuf>,
    rank: &GpuRank,
) -> Option<PathBuf> {
    let surviving: Vec<&PathBuf> = candidates
        .iter()
        .filter(|c| !rank.ignored.iter().any(|i| i == *c))
        .collect();

    for preferred in &rank.preferred {
        if let Some(hit) = surviving.iter().find(|c| **c == preferred) {
            return Some((*hit).clone());
        }
    }

    if let Some(d) = default_heuristic {
        if surviving.iter().any(|c| *c == d) {
            return Some(d.clone());
        }
    }

    surviving.first().map(|c| (*c).clone())
}
