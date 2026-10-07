// SPDX-License-Identifier: MIT OR Apache-2.0
//! The gallery's English Fluent catalogue; no runtime locale service in T0.
use toolkit::catalogue::Catalogue;

thread_local! {
    static CATALOGUE: Catalogue = Catalogue::english(
        include_str!("../../i18n/en/toolkit.ftl"),
    ).expect("valid English catalogue");
}

pub fn label(id: &str) -> String {
    format(id, &[])
}

pub fn format(id: &str, values: &[(&str, String)]) -> String {
    CATALOGUE.with(|catalogue| catalogue.format(id, values).expect("catalogue message"))
}
