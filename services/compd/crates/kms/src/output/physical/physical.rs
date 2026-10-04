//! Smithay Output + PhysicalProperties from connector info / EDID identity /
//! connector.kind orientation. (Ex wire.rs `new()` step 7.)
//! P1 default: with no readable EDID, properties match the original
//! hardcoded "Native"/"Monitor"/"Unknown" exactly.

use crate::edid::identity::identity::MonitorIdentity;
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::drm::control::connector;
use smithay::utils::{Size, Transform};
use std::sync::atomic::{AtomicU64, Ordering};

/// The KMS outputs' scale (`kms-live --scale S`), as `f64` bits; 0 = 1.0.
/// One value for every output; per-output scale is not supported yet.
static SCALE: AtomicU64 = AtomicU64::new(0);

/// Set the scale every output is created at. Call before the backend
/// assembles its displays; a value that is not finite and positive is 1.0.
pub fn set_scale(scale: f64) {
    let bits = if scale.is_finite() && scale > 0.0 { scale.to_bits() } else { 0 };
    SCALE.store(bits, Ordering::Relaxed);
}

/// The scale outputs are created at.
pub fn output_scale() -> f64 {
    match SCALE.load(Ordering::Relaxed) {
        0 => 1.0,
        bits => f64::from_bits(bits),
    }
}

/// `Scale` for `s`: clients without wp_fractional_scale render at the next
/// whole scale and are scaled down, as smithay's `Scale::Fractional` does.
fn scale_state(s: f64) -> Scale {
    Scale::Custom { advertised_integer: s.ceil().max(1.0) as i32, fractional: s }
}

pub fn create(info: &connector::Info, identity: &MonitorIdentity) -> Output {
    let (size_x, size_y) = info.size().unwrap_or((0, 0));
    Output::new(
        // Output NAME is the canonical DRM connector name (`eDP-1`, `HDMI-A-1`, …)
        // via `Interface::as_str()` — the SAME string the kernel and libinput use,
        // so `Device::output_name()` (a touchscreen's associated output) matches an
        // output by `name()`. It is unique per output (label / debug id / touch
        // routing). The IDENTITY that keys everything else (render, settings, prefs,
        // teleport) lives in `PhysicalProperties` below and falls back to the
        // connector when the EDID has no serial.
        format!("{}-{}", info.interface().as_str(), info.interface_id()),
        PhysicalProperties {
            size: Size::new(size_x as i32, size_y as i32),
            subpixel: Subpixel::Unknown,
            make: identity.make.clone().into(),
            model: identity.model.clone().into(),
            serial_number: identity.serial.clone().into(),
        },
    )
}

/// Apply the initial output state as the original did (preferred mode,
/// origin position), plus an optional panel-orientation transform from
/// `connector.kind`, at compd's scale ([`set_scale`]; 1.0 unless set, which
/// is exactly the original's `Custom { 1, 1.0 }`).
pub fn apply_initial_state(
    output: &Output,
    mode: Mode,
    orientation: Option<Transform>,
    position: (i32, i32),
) {
    output.set_preferred(mode);
    output.change_current_state(
        Some(mode),
        orientation,
        Some(scale_state(output_scale())),
        Some(position.into()),
    );
}

#[cfg(test)]
mod scale_tests {
    use super::*;

    #[test]
    fn the_advertised_integer_rounds_up_and_one_is_the_original() {
        assert!(matches!(scale_state(1.0), Scale::Custom { advertised_integer: 1, fractional } if fractional == 1.0));
        assert!(matches!(scale_state(2.5), Scale::Custom { advertised_integer: 3, fractional } if fractional == 2.5));
        assert!(matches!(scale_state(0.5), Scale::Custom { advertised_integer: 1, .. }));
    }
}
