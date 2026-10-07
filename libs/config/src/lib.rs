// SPDX-License-Identifier: MIT OR Apache-2.0

//! MixOS directories and configuration files.
//!
//! - [`path`] answers "where is the etc / var / run / share directory" by
//!   one rule ([`Dir`]): a `MIXOS_<DIR>` override, else `$MIXOS/<dir>`
//!   when a root is set, else the XDG directory for a user or the system
//!   directory for root. Code that needs the rule without the process
//!   environment resolves [`Dirs`] from an explicit [`Environment`].
//! - [`AppDirs`] resolves an application's existing config/state/cache roots,
//!   without creating directories. It honours absolute app overrides before
//!   project, XDG state and home defaults; `resolve_with` supports injected
//!   values while app-owned wrappers retain their filenames.
//! - [`parse`] and [`parse_file`] read a `*.conf.mix` file as strict data
//!   into a [`Value`] tree. They are the `strict` crate's, re-exported so a
//!   config reader needs one dependency; a typed reader uses
//!   `strict::from_file` directly.
//!
//! ```no_run
//! use config::{Dir, Value};
//!
//! let theme = config::path(Dir::Etc).join("theme.conf.mix");
//! let value: Value = config::parse_file(&theme)?;
//! let scheme = value.get("scheme").and_then(Value::as_str);
//! # Ok::<(), config::Error>(())
//! ```

mod app_dirs;
pub mod atomic;
mod dir;
pub mod node;
pub mod store;

pub use app_dirs::AppDirs;
pub use dir::{Dir, Dirs, Environment, path};
pub use strict::{Error, ErrorKind, Map, Value, parse, parse_file};
pub use strict::{
    from_file as load_conf_mix_path, from_str as from_conf_mix_str,
    to_string_pretty as to_conf_mix_string,
};
