//! Opt-in (Law 7, DOUBLE-GATED by the `mode-synthesize` cargo feature and
//! `SafetyEnable::mode_synthesize`): compute modes the monitor does not
//! advertise — CVT-RB timing generation, xorg-style modeline parsing, and
//! conversion into a kernel mode for the pipe. Only ever invoked when an
//! explicit `OutputProfile` requests a non-advertised mode; structurally
//! absent from the hot path otherwise.
//!
//! Failure policy: a malformed modeline in a preference is a configuration
//! error — the integration site panics (parse errors are local Results so
//! the panic message can carry the field).

#[cfg(feature = "mode-synthesize")]
pub use gated::*;

#[cfg(feature = "mode-synthesize")]
mod gated {
    #[derive(Debug, Clone, Copy)]
    pub struct SynthesizedTiming {
        pub clock_khz: u32,
        pub hdisplay: u16,
        pub hsync_start: u16,
        pub hsync_end: u16,
        pub htotal: u16,
        pub vdisplay: u16,
        pub vsync_start: u16,
        pub vsync_end: u16,
        pub vtotal: u16,
        /// (hsync positive, vsync positive)
        pub sync: (bool, bool),
    }

    /// CVT v1.2 reduced-blanking (the digital-display variant; what modern
    /// sinks expect for synthesized modes).
    pub fn cvt_rb(width: u16, height: u16, refresh: f64) -> SynthesizedTiming {
        const RB_MIN_VBLANK_US: f64 = 460.0;
        const RB_H_BLANK: u32 = 160;
        const RB_H_SYNC: u32 = 32;
        const RB_V_FPORCH: u32 = 3;
        const RB_MIN_V_BPORCH: u32 = 6;
        const CLOCK_STEP_KHZ: f64 = 250.0;

        let h_pixels_rnd = (width as u32 / 8) * 8;
        let v_lines = height as u32;

        let h_period_est =
            ((1.0 / refresh) - RB_MIN_VBLANK_US / 1_000_000.0) / (v_lines as f64) * 1_000_000.0;
        let vbi_lines = (RB_MIN_VBLANK_US / h_period_est).floor() as u32 + 1;
        let rb_min_vbi = RB_V_FPORCH + 1 + RB_MIN_V_BPORCH;
        let act_vbi_lines = vbi_lines.max(rb_min_vbi);

        let total_v_lines = act_vbi_lines + v_lines;
        let total_pixels = RB_H_BLANK + h_pixels_rnd;

        let act_pixel_freq_khz = (CLOCK_STEP_KHZ
            * ((refresh * total_v_lines as f64 * total_pixels as f64) / 1000.0 / CLOCK_STEP_KHZ)
                .floor()) as u32;

        let v_back_porch = act_vbi_lines - RB_V_FPORCH - 1;
        let v_sync_width = 1u32.max(act_vbi_lines.saturating_sub(RB_V_FPORCH + v_back_porch));

        let hsync_start = h_pixels_rnd + RB_H_BLANK / 2 - RB_H_SYNC;
        let hsync_end = hsync_start + RB_H_SYNC;
        let vsync_start = v_lines + RB_V_FPORCH;
        let vsync_end = vsync_start + v_sync_width;

        SynthesizedTiming {
            clock_khz: act_pixel_freq_khz,
            hdisplay: h_pixels_rnd as u16,
            hsync_start: hsync_start as u16,
            hsync_end: hsync_end as u16,
            htotal: total_pixels as u16,
            vdisplay: v_lines as u16,
            vsync_start: vsync_start as u16,
            vsync_end: vsync_end as u16,
            vtotal: total_v_lines as u16,
            // CVT-RB: hsync positive, vsync negative.
            sync: (true, false),
        }
    }

    /// Parse an xorg-style modeline body:
    /// "<clock MHz> hdisp hss hse htot vdisp vss vse vtot [+hsync|-hsync] [+vsync|-vsync]"
    /// Local Result: the caller (assembly) panics with the field on error.
    pub fn parse_modeline(s: &str) -> Result<SynthesizedTiming, String> {
        let fields: Vec<&str> = s.split_whitespace().collect();
        let numeric: Vec<&str> = fields
            .iter()
            .copied()
            .filter(|f| !f.starts_with('+') && !f.starts_with('-'))
            .collect();
        if numeric.len() != 9 {
            return Err(format!("modeline must have 9 numeric fields, got {}", numeric.len()));
        }
        let f = |i: usize| -> Result<f64, String> {
            numeric[i]
                .parse::<f64>()
                .map_err(|_| format!("invalid numeric field: {}", numeric[i]))
        };
        let hsync_pos = !fields.iter().any(|x| x.eq_ignore_ascii_case("-hsync"));
        let vsync_pos = !fields.iter().any(|x| x.eq_ignore_ascii_case("-vsync"));
        Ok(SynthesizedTiming {
            clock_khz: (f(0)? * 1000.0) as u32,
            hdisplay: f(1)? as u16,
            hsync_start: f(2)? as u16,
            hsync_end: f(3)? as u16,
            htotal: f(4)? as u16,
            vdisplay: f(5)? as u16,
            vsync_start: f(6)? as u16,
            vsync_end: f(7)? as u16,
            vtotal: f(8)? as u16,
            sync: (hsync_pos, vsync_pos),
        })
    }

    // Kernel uapi mode flags (stable ABI).
    const DRM_MODE_FLAG_PHSYNC: u32 = 1 << 0;
    const DRM_MODE_FLAG_NHSYNC: u32 = 1 << 1;
    const DRM_MODE_FLAG_PVSYNC: u32 = 1 << 2;
    const DRM_MODE_FLAG_NVSYNC: u32 = 1 << 3;

    /// Convert a synthesized timing into a kernel mode the pipe can take.
    pub fn to_drm_mode(t: SynthesizedTiming) -> smithay::reexports::drm::control::Mode {
        let vrefresh = ((t.clock_khz as u64 * 1000)
            / (t.htotal as u64 * t.vtotal as u64).max(1)) as u32;
        let mut name = [0 as std::ffi::c_char; 32];
        let label = format!("{}x{}@{}", t.hdisplay, t.vdisplay, vrefresh);
        for (i, b) in label.bytes().take(31).enumerate() {
            name[i] = b as std::ffi::c_char;
        }
        let raw = drm_ffi::drm_mode_modeinfo {
            clock: t.clock_khz,
            hdisplay: t.hdisplay,
            hsync_start: t.hsync_start,
            hsync_end: t.hsync_end,
            htotal: t.htotal,
            hskew: 0,
            vdisplay: t.vdisplay,
            vsync_start: t.vsync_start,
            vsync_end: t.vsync_end,
            vtotal: t.vtotal,
            vscan: 0,
            vrefresh,
            flags: if t.sync.0 { DRM_MODE_FLAG_PHSYNC } else { DRM_MODE_FLAG_NHSYNC }
                | if t.sync.1 { DRM_MODE_FLAG_PVSYNC } else { DRM_MODE_FLAG_NVSYNC },
            type_: 0,
            name,
        };
        smithay::reexports::drm::control::Mode::from(raw)
    }
}
