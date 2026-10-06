// SPDX-License-Identifier: MIT OR Apache-2.0
use fluent_bundle::{FluentBundle, FluentResource};
use std::{collections::BTreeMap, sync::OnceLock};
pub fn label(key: &str) -> String {
    static STRINGS: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    STRINGS
        .get_or_init(|| {
            let source = include_str!("../i18n/en/scene-editor.ftl");
            let resource =
                FluentResource::try_new(source.into()).expect("valid Scene Editor catalogue");
            let mut bundle = FluentBundle::new(vec!["en".parse().expect("locale")]);
            bundle
                .add_resource(resource)
                .expect("unique catalogue keys");
            source
                .lines()
                .filter_map(|line| line.split_once('=').map(|(key, _)| key.trim()))
                .map(|key| {
                    let pattern = bundle
                        .get_message(key)
                        .expect("message")
                        .value()
                        .expect("pattern");
                    (
                        key.to_owned(),
                        bundle
                            .format_pattern(pattern, None, &mut vec![])
                            .into_owned(),
                    )
                })
                .collect()
        })
        .get(key)
        .cloned()
        .unwrap_or_else(|| key.into())
}
