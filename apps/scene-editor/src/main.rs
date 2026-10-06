// SPDX-License-Identifier: MIT OR Apache-2.0
use scene_editor::{app::Settings, model};

fn parse(args: impl Iterator<Item=String>) -> Result<(Settings, Option<model::Selection>), String> {
    let mut settings = Settings::default();
    let mut selection = None;
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--service" | "--scenes" | "--comp" | "--noded-url" => {
                let value = args.next().filter(|v| !v.is_empty()).ok_or_else(|| format!("{arg} needs a value"))?;
                match arg.as_str() {
                    "--service" => settings.service = value,
                    "--scenes" => settings.scenes = value,
                    "--comp" => settings.comp = value,
                    _ => settings.url = value,
                }
            }
            _ => {
                if selection.is_some() { return Err("one launcher selection at a time".into()); }
                selection = Some(model::launch_selection(&arg)?);
            }
        }
    }
    for name in [&settings.service, &settings.scenes, &settings.comp] {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
            return Err("invalid service name".into());
        }
    }
    Ok((settings, selection))
}
fn main() {
    buildinfo::exit_on_version!();
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!("scene-editor — scene gallery, management and panel arrangement\nUsage: scene-editor [--service NAME] [--scenes NAME] [--comp NAME] [--noded-url URL] [mixos-scene-editor:VIEW/SCENE]\n--version reports exact build provenance");
        return;
    }
    let result = parse(std::env::args().skip(1)).and_then(|(settings, selection)| {
        if scene_editor::bus::probe(&settings.url, &settings.service) {
            return scene_editor::bus::forward(&settings.url, &settings.service, selection.as_ref());
        }
        match scene_editor::app::run(settings.clone(), selection.clone().unwrap_or_default()) {
            Err(_) if scene_editor::bus::probe(&settings.url, &settings.service) =>
                scene_editor::bus::forward(&settings.url, &settings.service, selection.as_ref()),
            result => result,
        }
    });
    if let Err(error) = result { eprintln!("scene-editor: {error}"); std::process::exit(1); }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cli_rejects_unknown_flags_and_unsafe_selection() {
        for args in [vec!["--scenes"], vec!["--service","bad name"], vec!["--unknown"], vec!["mixos-scene-editor:installed/../x"]] {
            assert!(parse(args.into_iter().map(str::to_owned)).is_err());
        }
        assert_eq!(parse(["--scenes","scenes.test","mixos-scene-editor:installed/panel"].into_iter().map(str::to_owned)).unwrap().0.scenes, "scenes.test");
    }
}
