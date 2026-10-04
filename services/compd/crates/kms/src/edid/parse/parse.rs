//! EDID blob read + minimal parse. Designated home; a full parser
//! (libdisplay-info bindings vs edid-rs) is decided at population time —
//! the minimal parse below covers identity extraction without new deps.

use smithay::backend::drm::DrmDevice;
use smithay::reexports::drm::control::{connector, property, Device};

#[derive(Debug, Clone)]
pub struct RawEdid(pub Vec<u8>);

/// Read the EDID property blob for a connector, if present.
pub fn read(drm: &DrmDevice, info: &connector::Info) -> Option<RawEdid> {
    let props = drm.get_properties(info.handle()).ok()?;
    for (prop, value) in props.iter() {
        let Ok(prop_info) = drm.get_property(*prop) else {
            continue;
        };
        if prop_info.name().to_str() != Ok("EDID") {
            continue;
        }
        if let property::Value::Blob(blob_id) =
            prop_info.value_type().convert_value(*value)
        {
            if blob_id == 0 {
                return None;
            }
            // Property blobs are fetched through the raw blob call.
            if let Ok(blob) = drm.get_property_blob(blob_id) {
                return Some(RawEdid(blob));
            }
        }
    }
    None
}

/// Minimal EDID block-0 fields needed for identity.
#[derive(Debug, Clone, Default)]
pub struct ParsedEdid {
    /// PNP manufacturer id, e.g. "DEL".
    pub manufacturer: String,
    pub product_code: u16,
    pub serial: u32,
    /// Monitor name descriptor (0xFC), if present.
    pub display_name: Option<String>,
}

pub fn parse(raw: &RawEdid) -> Option<ParsedEdid> {
    let d = &raw.0;
    if d.len() < 128 || d[0..8] != [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00] {
        return None;
    }
    let m = u16::from_be_bytes([d[8], d[9]]);
    let letter = |v: u16| ((v & 0x1F) as u8 + b'A' - 1) as char;
    let manufacturer: String = [letter(m >> 10), letter(m >> 5), letter(m)].iter().collect();
    let product_code = u16::from_le_bytes([d[10], d[11]]);
    let serial = u32::from_le_bytes([d[12], d[13], d[14], d[15]]);

    let mut display_name = None;
    for desc in 0..4 {
        let off = 54 + desc * 18;
        if d[off] == 0 && d[off + 1] == 0 && d[off + 3] == 0xFC {
            let name: String = d[off + 5..off + 18]
                .iter()
                .take_while(|b| **b != 0x0A)
                .map(|b| *b as char)
                .collect();
            display_name = Some(name.trim().to_string());
        }
    }
    Some(ParsedEdid {
        manufacturer,
        product_code,
        serial,
        display_name,
    })
}

// ───────────────────────────── HDR / color (M5) ─────────────────────────────
//
// Parsed from the EDID CTA-861 extension block(s): the HDR Static Metadata Data
// Block (extended tag 0x06 — supported EOTFs + desired luminance) and the
// Colorimetry Data Block (extended tag 0x05 — BT.2020 / DCI-P3 support), plus
// the display chromaticity from EDID base block 0. Hand-rolled (no new dep) —
// covers what `libdisplay-info`'s `hdr_static_metadata`/colorimetry give us for
// the output-signalling path. All decoding is per CTA-861-G / E-EDID.

/// Display chromaticity (CIE 1931 xy) from EDID base block bytes 25..35.
#[derive(Debug, Clone, Copy, Default)]
pub struct DisplayPrimaries {
    pub red: (f32, f32),
    pub green: (f32, f32),
    pub blue: (f32, f32),
    pub white: (f32, f32),
}

/// HDR Static Metadata Data Block (CTA-861 extended tag 0x06).
#[derive(Debug, Clone, Copy, Default)]
pub struct HdrStaticMetadata {
    /// ET_0: traditional gamma, SDR luminance range.
    pub eotf_sdr_gamma: bool,
    /// ET_1: traditional gamma, HDR luminance range.
    pub eotf_hdr_gamma: bool,
    /// ET_2: SMPTE ST 2084 (PQ) — the primary HDR indicator.
    pub eotf_pq: bool,
    /// ET_3: Hybrid Log-Gamma.
    pub eotf_hlg: bool,
    /// Desired content max luminance, cd/m² (decoded), if present.
    pub max_luminance: Option<f32>,
    /// Desired content max frame-average luminance, cd/m², if present.
    pub max_frame_avg_luminance: Option<f32>,
    /// Desired content min luminance, cd/m² (decoded), if present.
    pub min_luminance: Option<f32>,
}

/// Colorimetry Data Block (CTA-861 extended tag 0x05) — wide-gamut support.
#[derive(Debug, Clone, Copy, Default)]
pub struct ColorimetrySupport {
    pub bt2020_rgb: bool,
    pub bt2020_ycc: bool,
    pub bt2020_cycc: bool,
    pub dci_p3: bool,
}

/// Everything the output-signalling path needs from EDID for HDR.
#[derive(Debug, Clone, Copy, Default)]
pub struct HdrInfo {
    pub primaries: DisplayPrimaries,
    pub hdr: HdrStaticMetadata,
    pub colorimetry: ColorimetrySupport,
}

impl HdrInfo {
    /// True when the display advertises PQ HDR (ST 2084) — the gate for
    /// enabling the HDR output path.
    pub fn hdr_capable(&self) -> bool {
        self.hdr.eotf_pq
    }
}

/// Decode a 10-bit EDID chromaticity coordinate (high 8 bits + 2 low bits).
fn chroma(hi: u8, lo2: u8) -> f32 {
    (((hi as u16) << 2) | (lo2 as u16)) as f32 / 1024.0
}

fn parse_primaries(d: &[u8]) -> DisplayPrimaries {
    // Bytes 25..27 hold the low 2 bits; 27..35 the high 8 bits each.
    let rx = chroma(d[27], (d[25] >> 6) & 0x3);
    let ry = chroma(d[28], (d[25] >> 4) & 0x3);
    let gx = chroma(d[29], (d[25] >> 2) & 0x3);
    let gy = chroma(d[30], d[25] & 0x3);
    let bx = chroma(d[31], (d[26] >> 6) & 0x3);
    let by = chroma(d[32], (d[26] >> 4) & 0x3);
    let wx = chroma(d[33], (d[26] >> 2) & 0x3);
    let wy = chroma(d[34], d[26] & 0x3);
    DisplayPrimaries {
        red: (rx, ry),
        green: (gx, gy),
        blue: (bx, by),
        white: (wx, wy),
    }
}

/// Decode a CTA-861 luminance code: `50 * 2^(code/32)` cd/m².
fn lum(code: u8) -> f32 {
    50.0 * 2f32.powf(code as f32 / 32.0)
}

/// Parse HDR + colorimetry from a full EDID blob (base block 0 + CTA
/// extension blocks). Returns defaults (no HDR) if the blob is too short or
/// has no CTA extension.
pub fn parse_hdr(raw: &RawEdid) -> HdrInfo {
    let d = &raw.0;
    let mut info = HdrInfo::default();
    if d.len() < 128 {
        return info;
    }
    info.primaries = parse_primaries(d);

    // Walk each 128-byte extension block; CTA-861 blocks have tag 0x02.
    let ext_count = d[126] as usize;
    for i in 0..ext_count {
        let off = 128 * (i + 1);
        if off + 128 > d.len() || d[off] != 0x02 {
            continue;
        }
        parse_cta_block(&d[off..off + 128], &mut info);
    }
    info
}

/// Walk one CTA-861 extension block's data-block collection (offset 4 up to the
/// DTD start at byte 2), decoding the HDR-static-metadata and colorimetry blocks.
fn parse_cta_block(b: &[u8], info: &mut HdrInfo) {
    let dtd_start = b[2] as usize; // 0 ⇒ no DTDs; collection runs to end.
    let end = if dtd_start == 0 { 128 } else { dtd_start.min(128) };
    let mut p = 4;
    while p < end {
        let header = b[p];
        let tag = header >> 5;
        let len = (header & 0x1F) as usize; // payload length (excl. header byte)
        if len == 0 || p + 1 + len > 128 {
            break;
        }
        let payload = &b[p + 1..p + 1 + len];
        if tag == 0x07 && !payload.is_empty() {
            // Extended-tag block: payload[0] is the extended tag.
            match payload[0] {
                0x06 => parse_hdr_static_metadata(&payload[1..], &mut info.hdr),
                0x05 => parse_colorimetry(&payload[1..], &mut info.colorimetry),
                _ => {}
            }
        }
        p += 1 + len;
    }
}

fn parse_hdr_static_metadata(p: &[u8], hdr: &mut HdrStaticMetadata) {
    if p.is_empty() {
        return;
    }
    let eotf = p[0];
    hdr.eotf_sdr_gamma = eotf & 0x01 != 0;
    hdr.eotf_hdr_gamma = eotf & 0x02 != 0;
    hdr.eotf_pq = eotf & 0x04 != 0;
    hdr.eotf_hlg = eotf & 0x08 != 0;
    // p[1] = supported static-metadata descriptor types (bit0 = Type 1).
    // p[2..5] = optional desired content luminance codes (max, max-FALL, min).
    if p.len() > 2 {
        hdr.max_luminance = Some(lum(p[2]));
    }
    if p.len() > 3 {
        hdr.max_frame_avg_luminance = Some(lum(p[3]));
    }
    if p.len() > 4 {
        // Min luminance = max * (code/255)^2 / 100.
        if let Some(max) = hdr.max_luminance {
            let frac = p[4] as f32 / 255.0;
            hdr.min_luminance = Some(max * frac * frac / 100.0);
        }
    }
}

fn parse_colorimetry(p: &[u8], c: &mut ColorimetrySupport) {
    if p.is_empty() {
        return;
    }
    let b0 = p[0];
    c.bt2020_cycc = b0 & 0x20 != 0;
    c.bt2020_ycc = b0 & 0x40 != 0;
    c.bt2020_rgb = b0 & 0x80 != 0;
    if p.len() > 1 {
        // byte 1 bit7 = DCI-P3 (CTA-861-G).
        c.dci_p3 = p[1] & 0x80 != 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pq_eotf_and_luminance_decode() {
        let mut hdr = HdrStaticMetadata::default();
        // EOTF byte: PQ (bit2) + SDR (bit0); then SM byte; then max-lum code.
        parse_hdr_static_metadata(&[0x05, 0x01, 0xA0], &mut hdr);
        assert!(hdr.eotf_pq);
        assert!(hdr.eotf_sdr_gamma);
        assert!(!hdr.eotf_hlg);
        // 50 * 2^(160/32) = 50 * 32 = 1600 cd/m².
        assert!((hdr.max_luminance.unwrap() - 1600.0).abs() < 0.1);
    }

    #[test]
    fn colorimetry_bt2020() {
        let mut c = ColorimetrySupport::default();
        parse_colorimetry(&[0x80], &mut c); // BT.2020 RGB bit.
        assert!(c.bt2020_rgb);
        assert!(!c.bt2020_ycc);
    }
}
