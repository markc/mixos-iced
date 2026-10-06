// SPDX-License-Identifier: MIT OR Apache-2.0

//! Compile-time build provenance for MixOS binaries.
//!
//! A crate's semver alone is too weak a "what build is this?" signal: a
//! forgotten bump hides a real change. The truthful fingerprint is the git
//! sha plus the build time, and this crate gives every binary both, the same
//! way.
//!
//! The git sha embedded in a binary must be the sha of the repository that
//! binary was compiled from, captured at its own compile. So the capture runs
//! in the consumer's `build.rs` through [`emit`], and the values reach the
//! consumer through environment variables that [`build_info!`] reads at the
//! consumer's compile, never this crate's.
//!
//! # Usage
//!
//! In the consumer's `Cargo.toml`:
//! ```toml
//! [dependencies]
//! buildinfo = { path = "../../libs/buildinfo" }
//! [build-dependencies]
//! buildinfo = { path = "../../libs/buildinfo" }
//! ```
//! In `build.rs`:
//! ```ignore
//! fn main() { buildinfo::emit(); }
//! ```
//! As `main`'s first statement in every binary (the `--version` contract):
//! ```ignore
//! fn main() {
//!     buildinfo::exit_on_version!();
//!     // ...
//! }
//! ```
//! Elsewhere in the crate:
//! ```ignore
//! let info = buildinfo::build_info!();
//! println!("{}", info.line());
//! ```

/// Compile-time build provenance for a single crate.
///
/// Every field is `'static`, captured at compile time. Construct it with
/// [`build_info!`], which wires the right `env!`/`option_env!` sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildInfo {
    /// `CARGO_PKG_NAME` of the crate the macro expanded in.
    pub pkg: &'static str,
    /// `CARGO_PKG_VERSION` (semver).
    pub version: &'static str,
    /// Short (12 hex) git sha of the consumer repository's HEAD, or
    /// `"unknown"` when the consumer has no `build.rs` calling [`emit`], or
    /// no git.
    pub git_sha: &'static str,
    /// Full 40-hex object id of the consumer repository's HEAD. A provenance
    /// gate that compares a recorded commit needs the full form; equality
    /// with the short form can never hold. `"unknown"` under the same
    /// conditions as `git_sha`.
    pub git_sha_full: &'static str,
    /// Whether the consumer repository's working tree was dirty at build.
    pub git_dirty: bool,
    /// RFC3339 UTC build timestamp (honours `SOURCE_DATE_EPOCH`).
    pub build_time: &'static str,
}

impl BuildInfo {
    /// Human one-line form: `"<pkg> <version> (<sha>[-dirty], built <time>)"`.
    pub fn line(&self) -> String {
        format!(
            "{} {} ({}{}, built {})",
            self.pkg,
            self.version,
            self.git_sha,
            if self.git_dirty { "-dirty" } else { "" },
            self.build_time,
        )
    }

    /// Machine form of [`line`](Self::line): one JSON object carrying the
    /// full sha, for gates that compare a recorded 40-hex commit.
    pub fn json(&self) -> String {
        provenance_json(
            self.pkg,
            self.version,
            self.git_sha,
            self.git_sha_full,
            self.git_dirty,
            self.build_time,
        )
    }
}

/// The one renderer behind [`BuildInfo::json`] and the embedded marker, so
/// the two can never disagree about a field or its encoding.
fn provenance_json(
    pkg: &str,
    version: &str,
    git_sha: &str,
    git_sha_full: &str,
    git_dirty: bool,
    build_time: &str,
) -> String {
    format!(
        "{{\"component\":{},\"version\":{},\"git_sha\":{},\"git_sha_full\":{},\"git_dirty\":{},\"build_time\":{}}}",
        json_str(pkg),
        json_str(version),
        json_str(git_sha),
        json_str(git_sha_full),
        git_dirty,
        json_str(build_time),
    )
}

// ── The embedded provenance marker ──
//
// `--version --json` needs the binary to run. A fleet inventory must not run
// anything it merely finds on disk (an older build might not honour the
// flag), so every binary also carries its provenance as bytes:
//
//     MIXOS-BUILDINFO:1:{"component":…,"version":…,"git_sha":…,
//                        "git_sha_full":…,"git_dirty":…,"build_time":…}
//
// That is the JSON `--version --json` prints, rendered from the same build
// script run. `emit` puts it in `MIXOS_BUILDINFO_MARKER`; `build_info!` pins
// that string into the binary through a `#[used]` static it also references,
// so neither dead-code elimination, LTO nor `strip` (which drops symbols, not
// data) removes it. A reader finds the prefix and takes the flat JSON object
// up to its first `}` (no value can contain one). A library that itself calls
// `emit` and expands `build_info!` contributes its own marker too, so a
// binary may carry several: pick the one whose `component` names the
// binary's crate.

/// The marker prefix, version 1. Built from two halves so the prefix itself
/// is not a bare literal in this function's object code; rustc may still
/// fold it, which is harmless: a reader counts a candidate only where the
/// prefix is immediately followed by `{`, and keeps only JSON naming the
/// right crate.
pub fn marker_prefix() -> String {
    format!("{}{}", "MIXOS-BUILD", "INFO:1:")
}

/// Every well-formed marker JSON object in `bytes` (a binary's contents), in
/// file order, deduplicated. Malformed or truncated candidates are skipped.
pub fn find_markers(bytes: &[u8]) -> Vec<String> {
    let prefix = marker_prefix();
    let p = prefix.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i + p.len() <= bytes.len() {
        if &bytes[i..i + p.len()] != p {
            i += 1;
            continue;
        }
        let start = i + p.len();
        if bytes.get(start) == Some(&b'{') {
            let window = &bytes[start..bytes.len().min(start + 4096)];
            if let Some(e) = window.iter().position(|&b| b == b'}')
                && let Ok(s) = std::str::from_utf8(&window[..=e])
                && !out.iter().any(|o| o == s)
            {
                out.push(s.to_string());
            }
        }
        i = start;
    }
    out
}

/// A JSON string literal. The fields are compile-time crate metadata, but the
/// encoder still escapes everything JSON requires, so it stays dep-free.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Construct a [`BuildInfo`] for the **calling** crate.
///
/// `env!("CARGO_PKG_*")` and `option_env!("MIXOS_*")` resolve at the
/// expansion site, so the values describe the crate that invokes the macro,
/// not this one. `option_env!` (not `env!`) is used for the build.rs-set
/// variables so a crate without [`emit`] in its `build.rs` still compiles;
/// its provenance degrades to `"unknown"` / `false`, which is itself the
/// "no build.rs wired" signal.
#[macro_export]
macro_rules! build_info {
    () => {{
        // The embedded provenance marker (see `find_markers`): `#[used]` keeps
        // the static in the object file and the `black_box` reference keeps it
        // past the linker's section GC, so the marker lands in every binary
        // that expands this macro in a crate whose build.rs ran `emit()`.
        #[used]
        static MIXOS_BUILDINFO_MARKER: ::core::option::Option<&str> =
            option_env!("MIXOS_BUILDINFO_MARKER");
        let _ = ::core::hint::black_box(&MIXOS_BUILDINFO_MARKER);
        $crate::BuildInfo {
            pkg: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
            git_sha: option_env!("MIXOS_GIT_SHA").unwrap_or("unknown"),
            git_sha_full: option_env!("MIXOS_GIT_SHA_FULL").unwrap_or("unknown"),
            git_dirty: matches!(option_env!("MIXOS_GIT_DIRTY"), Some("1") | Some("true")),
            build_time: option_env!("MIXOS_BUILD_TIME").unwrap_or("unknown"),
        }
    }};
}

// ── The `--version` contract ──

/// Where in argv a version flag is honoured.
///
/// Every MixOS binary answers `--version`/`-V`; the only difference is how
/// far into argv it looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionScope {
    /// Anywhere before a bare `--`. The default: a daemon's exact systemd
    /// unit argv can be reused with the flag appended.
    Anywhere,
    /// `argv[1]` only, for binaries that forward the rest of their argv to
    /// something else: a terminal's `-e cmd --version` must run `cmd`.
    Leading,
}

/// Answer a version query, or `None` when `args` (the whole argv, program
/// name first) is not one. Scans with [`VersionScope::Anywhere`].
///
/// The answer is one line, `<pkg> <semver> (<sha12>[-dirty], built
/// <rfc3339>)` ([`BuildInfo::line`]). With `--json` also present, it is one
/// JSON object carrying the full sha instead.
///
/// `info` must come from [`build_info!`] expanded in the **binary's** crate,
/// or the sha describes this library rather than the program.
pub fn version_request(args: &[String], info: BuildInfo) -> Option<String> {
    version_request_scoped(args, info, VersionScope::Anywhere)
}

/// [`version_request`] with an explicit [`VersionScope`].
pub fn version_request_scoped(
    args: &[String],
    info: BuildInfo,
    scope: VersionScope,
) -> Option<String> {
    let rest = args.get(1..).unwrap_or(&[]);
    let (asked, json) = match scope {
        VersionScope::Leading => (
            matches!(rest.first().map(String::as_str), Some("--version" | "-V")),
            rest.get(1).map(String::as_str) == Some("--json"),
        ),
        VersionScope::Anywhere => {
            let before_dashdash = rest.iter().take_while(|a| a.as_str() != "--");
            let mut asked = false;
            let mut json = false;
            for arg in before_dashdash {
                match arg.as_str() {
                    "--version" | "-V" => asked = true,
                    "--json" => json = true,
                    _ => {}
                }
            }
            (asked, json)
        }
    };
    if !asked {
        return None;
    }
    Some(if json { info.json() } else { info.line() })
}

/// Handle `--version` for the running process: when argv asks for it, print
/// the answer to stdout and exit 0; otherwise return and let `main` carry on.
///
/// Call it as `main`'s first statement, before the async runtime (so never
/// under `#[tokio::main]`, which builds the runtime before the body runs),
/// config reads, logging, display checks, Bus connections or windows, so the
/// answer never depends on startup succeeding and a second copy of a running
/// program answers truthfully. Prefer the [`exit_on_version!`] macro, which
/// expands [`build_info!`] in the caller. Non-UTF-8 arguments are compared
/// lossily; they can never spell a version flag, so nothing is lost.
pub fn exit_on_version(info: BuildInfo, scope: VersionScope) {
    let args: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if let Some(text) = version_request_scoped(&args, info, scope) {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        // A closed stdout (`bin --version | head -0`) is not a reason to
        // panic; the exit status is still the contract.
        let _ = writeln!(out, "{text}");
        let _ = out.flush();
        std::process::exit(0);
    }
}

/// `main`'s first line in every MixOS binary: answer `--version`/`-V` and
/// exit 0, or fall through. Expands [`build_info!`] here, in the binary's
/// crate, so the reported sha is the binary's.
///
/// `exit_on_version!()` scans the whole argv ([`VersionScope::Anywhere`]);
/// `exit_on_version!(leading)` looks at `argv[1]` only, for programs that
/// forward their argv.
#[macro_export]
macro_rules! exit_on_version {
    () => {
        $crate::exit_on_version($crate::build_info!(), $crate::VersionScope::Anywhere)
    };
    (leading) => {
        $crate::exit_on_version($crate::build_info!(), $crate::VersionScope::Leading)
    };
}

// ── build.rs helper ──

/// Emit `cargo:rustc-env=MIXOS_{GIT_SHA,GIT_SHA_FULL,GIT_DIRTY,BUILD_TIME,
/// BUILDINFO_MARKER}` for the consumer crate. Call from the consumer's
/// `build.rs` `main()`.
///
/// - `git_sha`: `git rev-parse --short=12 HEAD` in the consumer repository,
///   or `"unknown"` when git is unavailable or there is no repository (a
///   tarball build). Never fails the build.
/// - `git_dirty`: `git status --porcelain` non-empty (tracked modifications
///   or untracked non-ignored files), repository-wide, so a dirty dependency
///   crate also flags the binary, which is correct: it was built against
///   uncommitted code. The bit recomputes when the consumer package's own
///   `src/` or `Cargo.toml` change, so between rebuilds it can lag an
///   uncommitted edit elsewhere in the repository. Release builds come from
///   a clean checkout, where it is reliably `false`.
/// - `build_time`: RFC3339 UTC. Honours `SOURCE_DATE_EPOCH` (so a
///   reproducible build can pin it); falls back to the wall clock.
///
/// Re-run triggers are emitted so the sha tracks new commits and the
/// timestamp follows the epoch override.
pub fn emit() {
    let sha = git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let sha_full = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let dirty = git(&["status", "--porcelain"])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

    println!("cargo:rustc-env=MIXOS_GIT_SHA={sha}");
    println!("cargo:rustc-env=MIXOS_GIT_SHA_FULL={sha_full}");
    println!(
        "cargo:rustc-env=MIXOS_GIT_DIRTY={}",
        if dirty { 1 } else { 0 }
    );
    let built = build_time();
    println!("cargo:rustc-env=MIXOS_BUILD_TIME={built}");
    // The embedded marker: the JSON `--version --json` prints, from the same
    // values, for byte-level readers that must not execute the binary.
    let pkg = std::env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "unknown".to_string());
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_string());
    println!(
        "cargo:rustc-env=MIXOS_BUILDINFO_MARKER={}{}",
        marker_prefix(),
        provenance_json(&pkg, &version, &sha, &sha_full, dirty, &built)
    );

    // Emitting any `rerun-if-changed` disables Cargo's default "rerun on any
    // package-file change" scan, so the source watch is re-added here;
    // otherwise `git_dirty`/`build_time` go stale on an uncommitted edit. Git
    // metadata is watched too, which Cargo's package scan never sees, so the
    // sha tracks new commits.
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        println!("cargo:rerun-if-changed={manifest}/src");
        println!("cargo:rerun-if-changed={manifest}/Cargo.toml");
    }
    // HEAD lives in the per-worktree git dir; branch refs and packed-refs
    // live in the common dir. They differ under a linked worktree, so both
    // are watched; watching only the per-worktree dir would embed a stale sha
    // after a same-branch commit in a worktree. Best effort: no git means no
    // watches and a fixed "unknown" sha.
    if let Some(git_dir) = git(&["rev-parse", "--git-dir"]) {
        println!("cargo:rerun-if-changed={}/HEAD", git_dir.trim());
    }
    if let Some(common) = git(&["rev-parse", "--git-common-dir"]) {
        let c = common.trim();
        println!("cargo:rerun-if-changed={c}/packed-refs");
        if let Some(reff) = git(&["symbolic-ref", "--quiet", "HEAD"]) {
            println!("cargo:rerun-if-changed={c}/{}", reff.trim());
        }
    }
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
}

fn git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn build_time() -> String {
    let epoch = match std::env::var("SOURCE_DATE_EPOCH") {
        // Set and valid: pin it (reproducible build).
        Ok(raw) if raw.trim().parse::<i64>().is_ok() => raw.trim().parse::<i64>().unwrap(),
        // Set but garbage: the caller wants a pinned time, and silently
        // falling back to the wall clock would defeat reproducibility. Warn
        // and use a deterministic 0 instead.
        Ok(_) => {
            println!(
                "cargo:warning=SOURCE_DATE_EPOCH is set but not a valid integer; \
                 using epoch 0 for MIXOS_BUILD_TIME (deterministic, not wall-clock)"
            );
            0
        }
        // Unset: an ordinary build; the wall clock is the freshness signal.
        Err(_) => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    };
    rfc3339_utc(epoch)
}

/// Format a Unix epoch (seconds) as RFC3339 UTC, dep-free.
///
/// Uses Howard Hinnant's `civil_from_days` algorithm (public domain), so no
/// `chrono`/`time` dependency is pulled into every binary's build.
pub fn rfc3339_utc(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let rem = epoch_secs.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // civil_from_days: days since 1970-01-01 to (year, month, day).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Current UTC time for runtime registration provenance.
pub fn now_rfc3339() -> String {
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    rfc3339_utc(epoch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_zero_is_unix_epoch() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn known_timestamp_formats_correctly() {
        assert_eq!(rfc3339_utc(1_780_272_000), "2026-06-01T00:00:00Z");
        // A mid-day, mid-month value with all fields non-zero.
        assert_eq!(rfc3339_utc(1_626_352_496), "2021-07-15T12:34:56Z");
    }

    #[test]
    fn leap_day_handled() {
        assert_eq!(rfc3339_utc(1_582_934_400), "2020-02-29T00:00:00Z");
    }

    fn demo() -> BuildInfo {
        BuildInfo {
            pkg: "demo",
            version: "1.2.3",
            git_sha: "abc123def456",
            git_sha_full: "abc123def456abc123def456abc123def456abc1",
            git_dirty: false,
            build_time: "2026-06-01T00:00:00Z",
        }
    }

    fn argv(rest: &[&str]) -> Vec<String> {
        std::iter::once("demo")
            .chain(rest.iter().copied())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn build_info_line_includes_dirty_marker() {
        let info = BuildInfo {
            git_dirty: true,
            ..demo()
        };
        assert_eq!(
            info.line(),
            "demo 1.2.3 (abc123def456-dirty, built 2026-06-01T00:00:00Z)"
        );
    }

    #[test]
    fn version_request_answers_both_spellings_with_one_line() {
        let want = "demo 1.2.3 (abc123def456, built 2026-06-01T00:00:00Z)";
        for flag in ["--version", "-V"] {
            let out = version_request(&argv(&[flag]), demo()).expect("a version request");
            assert_eq!(out, want);
            assert_eq!(out.lines().count(), 1);
        }
    }

    #[test]
    fn anywhere_scope_honours_the_flag_after_a_unit_argv_but_not_after_dashdash() {
        assert!(version_request(&argv(&["serve", "--config", "x", "--version"]), demo()).is_some());
        assert!(version_request(&argv(&["run", "--", "cmd", "--version"]), demo()).is_none());
        assert!(version_request(&argv(&[]), demo()).is_none());
        assert!(version_request(&argv(&["--versionx"]), demo()).is_none());
        // argv[0] is the program, never a flag.
        assert!(version_request(&["--version".to_string()], demo()).is_none());
    }

    #[test]
    fn leading_scope_only_reads_argv1() {
        let s = VersionScope::Leading;
        assert!(version_request_scoped(&argv(&["-V"]), demo(), s).is_some());
        assert!(version_request_scoped(&argv(&["-e", "cmd", "--version"]), demo(), s).is_none());
    }

    #[test]
    fn json_form_carries_the_full_sha_and_escapes() {
        let out = version_request(&argv(&["--version", "--json"]), demo()).expect("json");
        assert_eq!(
            out,
            "{\"component\":\"demo\",\"version\":\"1.2.3\",\"git_sha\":\"abc123def456\",\
             \"git_sha_full\":\"abc123def456abc123def456abc123def456abc1\",\"git_dirty\":false,\
             \"build_time\":\"2026-06-01T00:00:00Z\"}"
        );
        assert_eq!(json_str("a\"b\\c\n"), "\"a\\\"b\\\\c\\u000a\"");
        // Leading scope reads --json only from argv[2].
        let s = VersionScope::Leading;
        let leading = version_request_scoped(&argv(&["--version", "--json"]), demo(), s);
        assert!(leading.expect("json").starts_with('{'));
        let not_json = version_request_scoped(&argv(&["--version", "x", "--json"]), demo(), s);
        assert!(!not_json.expect("line").starts_with('{'));
    }

    #[test]
    fn build_info_macro_expands_in_this_crate() {
        // Without a build.rs setting MIXOS_*, the macro degrades to
        // "unknown"/false but still yields this crate's pkg/version.
        let info = build_info!();
        assert_eq!(info.pkg, "buildinfo");
        assert!(!info.version.is_empty());
        assert_eq!(info.git_sha, "unknown");
    }

    #[test]
    fn marker_json_is_the_version_json() {
        let info = demo();
        let marker = format!("{}{}", marker_prefix(), info.json());
        let mut bytes = b"\x00\x7fELF junk".to_vec();
        bytes.extend_from_slice(marker.as_bytes());
        bytes.extend_from_slice(b"\x00more rodata");
        assert_eq!(find_markers(&bytes), vec![info.json()]);
    }

    #[test]
    fn find_markers_skips_false_starts_and_dedupes() {
        let json = demo().json();
        let p = marker_prefix();
        let mut bytes = Vec::new();
        // A bare prefix with no object after it, and one never closed within
        // the window, are both skipped rather than misread.
        bytes.extend_from_slice(p.as_bytes());
        bytes.extend_from_slice(b"not json");
        bytes.extend_from_slice(format!("{p}{json}").as_bytes());
        bytes.extend_from_slice(format!("{p}{json}").as_bytes());
        let other = BuildInfo {
            pkg: "other",
            ..demo()
        }
        .json();
        bytes.extend_from_slice(format!("{p}{other}").as_bytes());
        bytes.extend_from_slice(p.as_bytes());
        bytes.extend_from_slice(b"{\"unterminated\":");
        assert_eq!(find_markers(&bytes), vec![json, other]);
    }

    #[test]
    fn marker_prefix_is_versioned() {
        assert_eq!(marker_prefix(), "MIXOS-BUILDINFO:1:");
    }
}
