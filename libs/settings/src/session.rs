// SPDX-License-Identifier: MIT OR Apache-2.0
//! Session binding shared by native consumers. Explicit environment values
//! win; the package session authority defaults to this machine's hostname.
use crate::{Binding, Diagnostic};

pub fn binding() -> Result<Binding, Diagnostic> {
    let instance = match std::env::var("MIXOS_SETTINGS_INSTANCE") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map_err(|error| {
                Diagnostic::new("missing_session_binding", "instance", error.to_string())
            })?
            .trim()
            .to_owned(),
        Err(error) => {
            return Err(Diagnostic::new(
                "invalid_session_binding",
                "MIXOS_SETTINGS_INSTANCE",
                error.to_string(),
            ));
        }
    };
    let profile = match std::env::var("MIXOS_SETTINGS_PROFILE") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => "default".to_owned(),
        Err(error) => {
            return Err(Diagnostic::new(
                "invalid_session_binding",
                "MIXOS_SETTINGS_PROFILE",
                error.to_string(),
            ));
        }
    };
    let binding = Binding { instance, profile };
    binding.validate()?;
    Ok(binding)
}
