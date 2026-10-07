// SPDX-License-Identifier: MIT OR Apache-2.0

//! Caller-owned Fluent catalogues for native hosts and shared widgets.
//! This formats explicit message IDs; it owns no locale service or file I/O.

use std::fmt;

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource};

pub struct Catalogue {
    bundle: FluentBundle<FluentResource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Invalid(String),
    Missing(String),
    Formatting(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for Error {}

impl Catalogue {
    /// Build an English catalogue from embedded source. Owners keep their
    /// own message IDs and resource; full Fluent syntax is parsed by Fluent.
    pub fn english(source: &str) -> Result<Self, Error> {
        let resource = FluentResource::try_new(source.to_owned())
            .map_err(|(_, errors)| Error::Invalid(format!("{errors:?}")))?;
        let mut bundle = FluentBundle::new(vec!["en".parse().expect("English locale")]);
        bundle.set_use_isolating(false);
        bundle
            .add_resource(resource)
            .map_err(|errors| Error::Invalid(format!("{errors:?}")))?;
        Ok(Self { bundle })
    }

    pub fn label(&self, id: &str) -> Result<String, Error> {
        self.format(id, &[])
    }

    pub fn format(&self, id: &str, values: &[(&str, String)]) -> Result<String, Error> {
        let pattern = self
            .bundle
            .get_message(id)
            .and_then(|message| message.value())
            .ok_or_else(|| Error::Missing(id.to_owned()))?;
        let mut args = FluentArgs::new();
        for (name, value) in values {
            args.set(*name, value.as_str());
        }
        let mut errors = Vec::new();
        let result = self
            .bundle
            .format_pattern(pattern, Some(&args), &mut errors)
            .into_owned();
        if errors.is_empty() {
            Ok(result)
        } else {
            Err(Error::Formatting(format!("{errors:?}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_fluent_patterns_and_terms_are_resolved_without_key_discovery() {
        let catalogue = Catalogue::english(
            "-app = Editor\nplain = Ready\nstatus =\n    { -app }: { $condition }\n",
        )
        .unwrap();
        assert_eq!(catalogue.label("plain").unwrap(), "Ready");
        assert_eq!(
            catalogue
                .format("status", &[("condition", "Connecting".into())])
                .unwrap(),
            "Editor: Connecting"
        );
        assert!(matches!(catalogue.label("missing"), Err(Error::Missing(_))));
        assert!(matches!(
            catalogue.label("status"),
            Err(Error::Formatting(_))
        ));
        assert!(Catalogue::english("bad = {\n").is_err());
        assert!(Catalogue::english("one = First\none = Second\n").is_err());
    }
}
