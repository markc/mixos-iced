// SPDX-License-Identifier: MIT OR Apache-2.0
use toolkit::catalogue::Catalogue;
thread_local! {
    static CATALOGUE: Catalogue = Catalogue::english(include_str!("../i18n/en/scene-editor.ftl"))
        .expect("valid Scene Editor English catalogue");
}
pub fn label(key: &str) -> String {
    CATALOGUE.with(|catalogue| {
        catalogue
            .label(key)
            .expect("Scene Editor catalogue message")
    })
}
