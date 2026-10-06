// SPDX-License-Identifier: MIT OR Apache-2.0
//! Cap's CLI starts a native window or a Bus-only agent service.
use cap::{bus, capture, verbs};
use std::path::PathBuf;
const HELP: &str = "cap — native screenshots and editable annotations\nUsage: cap [OPTIONS] [IMAGE]\n  --headless        Bus service without a window\n  --service NAME    service name (default cap)\n  --comp NAME       compositor service (default comp)\n  --noded-url URL   native broker endpoint\n  --version         version and exact build provenance";
struct Args {
    headless: bool,
    service: String,
    comp: String,
    url: String,
    path: Option<PathBuf>,
}
fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args {
        headless: false,
        service: verbs::SERVICE.into(),
        comp: std::env::var("MIXOS_COMP_SERVICE").unwrap_or_else(|_| "comp".into()),
        url: ::bus::client_helpers::resolve_noded_url(),
        path: None,
    };
    let mut args = args.peekable();
    let mut positional = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" if !positional => positional = true,
            "--headless" if !positional => out.headless = true,
            "--service" | "--comp" | "--noded-url" if !positional => {
                let value = args.next().ok_or_else(|| format!("{arg} needs a value"))?;
                if value.is_empty() {
                    return Err(format!("{arg} cannot be empty"));
                }
                match arg.as_str() {
                    "--service" => out.service = value,
                    "--comp" => out.comp = value,
                    _ => out.url = value,
                }
            }
            flag if !positional && flag.starts_with('-') => {
                return Err(format!("unknown option {flag}"));
            }
            _ => {
                if out.path.is_some() {
                    return Err("open one image at a time".into());
                }
                out.path = Some(capture::absolute(&arg)?)
            }
        }
    }
    for name in [&out.service, &out.comp] {
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        {
            return Err("invalid Bus service name".into());
        }
    }
    Ok(out)
}
fn main() {
    buildinfo::exit_on_version!();
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return;
    }
    let args = match parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("cap: {e}");
            std::process::exit(2)
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("CAP_LOG")
                .unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let paths = args
        .path
        .as_ref()
        .map(|p| vec![p.to_string_lossy().into_owned()])
        .unwrap_or_default();
    if !args.headless && bus::probe_running(&args.url, &args.service) {
        finish(bus::forward_open(&args.url, &args.service, &paths));
        return;
    }
    let result = if args.headless {
        cap::headless::run(&args.service, &args.url, &args.comp, args.path)
    } else {
        cap::app::run(&args.service, &args.url, &args.comp, args.path)
    };
    if result.is_err() && !args.headless && bus::probe_running(&args.url, &args.service) {
        finish(bus::forward_open(&args.url, &args.service, &paths))
    } else {
        finish(result)
    }
}
fn finish(result: Result<(), String>) {
    if let Err(error) = result {
        eprintln!("cap: {error}");
        std::process::exit(1)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cli_is_strict() {
        assert!(parse(["--comp".into()].into_iter()).is_err());
        assert!(parse(["--service".into(), "bad name".into()].into_iter()).is_err());
        assert!(parse(["a.png".into(), "b.png".into()].into_iter()).is_err());
        assert!(
            parse(["--headless".into(), "--comp".into(), "comp.test".into()].into_iter())
                .unwrap()
                .headless
        );
    }
}
