// SPDX-License-Identifier: MIT OR Apache-2.0
use fluent_bundle::{FluentBundle, FluentResource};
use std::{collections::BTreeMap, sync::OnceLock};
fn catalogue() -> &'static BTreeMap<String, String> {
    static STRINGS: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    STRINGS.get_or_init(|| {
        let resource = FluentResource::try_new(include_str!("../i18n/en/cap.ftl").into())
            .expect("valid Cap catalogue");
        let mut bundle = FluentBundle::new(vec!["en".parse().expect("locale")]);
        let keys: Vec<String> = include_str!("../i18n/en/cap.ftl")
            .lines()
            .filter_map(|line| line.split_once('=').map(|(key, _)| key.trim().to_owned()))
            .collect();
        bundle.add_resource(resource).expect("unique Cap strings");
        keys.into_iter()
            .map(|key| {
                let value = bundle
                    .get_message(&key)
                    .expect("message")
                    .value()
                    .expect("pattern");
                let value = bundle.format_pattern(value, None, &mut vec![]).into_owned();
                (key, value)
            })
            .collect()
    })
}
pub fn label_ref(key: &'static str) -> &'static str {
    catalogue().get(key).map(String::as_str).unwrap_or(key)
}
pub fn label(key: &str) -> String {
    catalogue().get(key).cloned().unwrap_or_else(|| key.into())
}
