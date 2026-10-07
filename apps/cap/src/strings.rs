// SPDX-License-Identifier: MIT OR Apache-2.0
//! Cap's English catalogue through the shared toolkit formatter. Message IDs
//! are explicit call sites; there is no line-key scanner. The Fluent bundle
//! is thread-local (it is neither Send nor Sync); the finite placeholder map
//! is ordinary Sync data.
use std::{collections::BTreeMap, sync::OnceLock};
use toolkit::catalogue::Catalogue;
thread_local! {
    static CATALOGUE: Catalogue =
        Catalogue::english(include_str!("../i18n/en/cap.ftl")).expect("valid Cap catalogue");
}
pub fn label(key: &str) -> String {
    CATALOGUE.with(|catalogue| catalogue.label(key).unwrap_or_else(|_| key.into()))
}
/// A finite owned set of placeholders that must outlive a view borrow. Every
/// other string goes through [`label`].
pub fn label_ref(key: &'static str) -> &'static str {
    static PLACEHOLDERS: OnceLock<BTreeMap<&'static str, String>> = OnceLock::new();
    let placeholders = PLACEHOLDERS.get_or_init(|| {
        ["text-placeholder", "text-size-range"]
            .into_iter()
            .map(|key| (key, label(key)))
            .collect()
    });
    placeholders
        .get(key)
        .map(String::as_str)
        .unwrap_or_else(|| panic!("label_ref is a finite owned set; use label for {key}"))
}
