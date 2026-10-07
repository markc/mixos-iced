// SPDX-License-Identifier: MIT OR Apache-2.0
use busviewer::{app::Settings, bus};
fn parse(args: impl Iterator<Item = String>) -> Result<Settings, String> {
    let mut settings = Settings::default();
    let mut args = args;
    while let Some(arg) = args.next() {
        let value = args
            .next()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{arg} needs a value"))?;
        match arg.as_str() {
            "--noded-url" => settings.url = value,
            "--service" => settings.service = value,
            "--comp" => settings.comp = value,
            _ => return Err(format!("unknown option: {arg}")),
        }
    }
    for name in [&settings.service, &settings.comp] {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        {
            return Err("invalid service name".into());
        }
    }
    Ok(settings)
}
fn main() {
    buildinfo::exit_on_version!(leading);
    if matches!(std::env::args().nth(1).as_deref(), Some("--help" | "-h")) {
        println!(
            "busviewer — native ABP service and verb browser\nUsage: busviewer [--noded-url URL] [--service NAME] [--comp NAME]"
        );
        return;
    }
    let result = parse(std::env::args().skip(1)).and_then(|settings| {
        if bus::probe(&settings.url, &settings.service) {
            match bus::forward(&settings.url, &settings.service) {
                Ok(()) => return Ok(()),
                Err(error) if bus::probe(&settings.url, &settings.service) => return Err(error),
                Err(_) => {} // The previous instance exited between probe and activation.
            }
        }
        match busviewer::app::run(settings.clone()) {
            Err(_) if bus::probe(&settings.url, &settings.service) => {
                bus::forward(&settings.url, &settings.service)
            }
            result => result,
        }
    });
    if let Err(error) = result {
        eprintln!("busviewer: {error}");
        std::process::exit(1);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unknown_missing_and_unsafe_options() {
        for args in [
            vec!["--noded-url"],
            vec!["--bogus", "x"],
            vec!["--service", "bad name"],
        ] {
            assert!(parse(args.into_iter().map(str::to_owned)).is_err());
        }
        assert_eq!(
            parse(
                ["--service", "busviewer.test"]
                    .into_iter()
                    .map(str::to_owned)
            )
            .unwrap()
            .service,
            "busviewer.test"
        );
    }
}
