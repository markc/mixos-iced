// SPDX-License-Identifier: MIT OR Apache-2.0
use clap::{Parser, Subcommand};
use settings::{Binding, Desktop};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "settingsd",
    version,
    about = "Native ABP desktop settings authority"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(clap::Args)]
struct Profile {
    #[arg(long)]
    root: Option<PathBuf>,
    #[arg(long)]
    instance: String,
    #[arg(long, default_value = "default")]
    profile: String,
}
impl Profile {
    fn resolve(self) -> anyhow::Result<(PathBuf, Binding)> {
        let binding = Binding {
            instance: self.instance,
            profile: self.profile,
        };
        binding.validate().map_err(|e| anyhow::anyhow!(e.message))?;
        let root = self.root.unwrap_or_else(|| {
            config::path(config::Dir::Etc)
                .join("settings")
                .join(&binding.profile)
        });
        Ok((root, binding))
    }
}
#[derive(Subcommand)]
enum Command {
    /// Explicitly create a new profile. Never overwrites existing state.
    Init(Profile),
    /// Create a first-run session profile, or validate existing state unchanged.
    Seed(Profile),
    /// Serve an established profile; missing/unsupported state fails visibly.
    Serve(Profile),
}
fn main() -> anyhow::Result<()> {
    buildinfo::exit_on_version!();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init();
    match Cli::parse().command {
        Command::Init(profile) => {
            let (root, binding) = profile.resolve()?;
            let (_store, accepted) =
                settingsd::store::Store::create(&root, binding, Desktop::default())?;
            println!(
                "{}",
                serde_json::json!({"status":"initialised","binding":accepted.binding,"incarnation":accepted.incarnation,"revision":accepted.revision})
            );
            Ok(())
        }
        Command::Seed(profile) => {
            let (root, binding) = profile.resolve()?;
            let (_store, accepted) = settingsd::store::Store::seed(&root, binding)?;
            println!("{}", serde_json::json!({"status":"seeded","binding":accepted.binding,"incarnation":accepted.incarnation,"revision":accepted.revision}));
            Ok(())
        }
        Command::Serve(profile) => {
            let (root, binding) = profile.resolve()?;
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(settingsd::service::serve(root, binding))
        }
    }
}
