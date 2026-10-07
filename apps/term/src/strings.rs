// SPDX-License-Identifier: MIT OR Apache-2.0
use toolkit::catalogue::Catalogue;

thread_local! {
    static CATALOGUE: Catalogue = Catalogue::english(include_str!("../i18n/en/term.ftl"))
        .expect("valid Term English catalogue");
}

pub fn label(key: &str) -> String {
    CATALOGUE.with(|catalogue| {
        catalogue
            .label(key)
            .expect("Term catalogue message")
    })
}

pub fn format(key: &str, values: &[(&str, String)]) -> String {
    CATALOGUE.with(|catalogue| {
        catalogue
            .format(key, values)
            .expect("Term catalogue message")
    })
}
