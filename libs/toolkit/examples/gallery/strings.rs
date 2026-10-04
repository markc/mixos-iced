// SPDX-License-Identifier: MIT OR Apache-2.0
//! The gallery's English Fluent catalogue; no runtime locale service in T0.
use fluent_bundle::{FluentArgs, FluentBundle, FluentResource};

thread_local! {
    static CATALOGUE: FluentBundle<FluentResource> = {
        let resource = FluentResource::try_new(
            include_str!("../../i18n/en/toolkit.ftl").to_owned(),
        ).expect("valid English catalogue");
        let mut bundle = FluentBundle::new(vec!["en".parse().expect("English locale")]);
        bundle.set_use_isolating(false);
        bundle.add_resource(resource).expect("unique messages");
        bundle
    };
}

pub fn label(id: &str) -> String {
    format(id, &[])
}

pub fn format(id: &str, values: &[(&str, String)]) -> String {
    CATALOGUE.with(|bundle| {
        let mut args = FluentArgs::new();
        for (name, value) in values {
            args.set(*name, value.as_str());
        }
        let pattern = bundle
            .get_message(id)
            .and_then(|message| message.value())
            .expect("catalogue message");
        let mut errors = Vec::new();
        let result = bundle
            .format_pattern(pattern, Some(&args), &mut errors)
            .into_owned();
        assert!(errors.is_empty(), "Fluent errors: {errors:?}");
        result
    })
}
