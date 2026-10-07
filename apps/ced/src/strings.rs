// SPDX-License-Identifier: MIT OR Apache-2.0

use toolkit::catalogue::Catalogue;

thread_local! {
    static CATALOGUE: Catalogue = Catalogue::english(include_str!("../i18n/en/ced.ftl"))
        .expect("valid Ced English catalogue");
}

pub(crate) fn label(id: &str) -> String {
    CATALOGUE.with(|catalogue| catalogue.label(id).expect("Ced catalogue message"))
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
            assert!(!label(id).is_empty());
        }
    }
}
