// SPDX-License-Identifier: MIT OR Apache-2.0
//! The shared Fluent catalogue for the persistent provenance/fault status
//! line: which settings generation the window presents and what its Bus
//! registration state is. The same vocabulary ced's catalogue carries.

use toolkit::catalogue::Catalogue;

thread_local! {
    static CATALOGUE: Catalogue = Catalogue::english(include_str!("../i18n/en/dopus.ftl"))
        .expect("valid DOpus English catalogue");
}

pub(crate) fn label(id: &str) -> String {
    CATALOGUE.with(|catalogue| catalogue.label(id).expect("DOpus catalogue message"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_settings_and_connection_labels_exist() {
        for id in [
            "settings-current",
            "settings-cached",
            "settings-embedded",
            "settings-retained",
            "settings-last-good",
            "settings-bootstrap",
            "bus-connected",
            "bus-connecting",
            "bus-disconnected",
            "bus-refused",
        ] {
            assert!(!label(id).is_empty(), "{id} must be a catalogue message");
        }
    }
}
