// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `generic` gate: toolkit is written for any iced project, so nothing
//! in it may name the project it ships with, and its dependency closure may
//! reach no crate of that project. Two checks:
//!
//! 1. no file under `src`, `examples`, `tests` (other than this file),
//!    `i18n`, `README.md` or `Cargo.toml` contains a `FORBIDDEN` term,
//!    case-insensitively;
//! 2. toolkit's normal-dependency closure (default and all features)
//!    contains no workspace member but toolkit, and every path dependency in
//!    it lives under the workspace's `vendor/` (the vendored iced family).
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Terms that mark project-specific code: the project's names, its install
/// prefix, its environment-variable prefix and its app-ID namespace.
const FORBIDDEN: [&str; 5] = ["mixos", "cosmix", "/opt/", "mixos_", "dev.mixos"];

/// The one line cargo's workspace bookkeeping needs (`[package.metadata.*]`,
/// ignored by cargo and by any project that takes the crate). Compared after
/// trimming, so it never matches a longer line.
const ALLOWED_LINES: [&str; 1] = ["[package.metadata.mixos]"];

const SCANNED: [&str; 6] = ["src", "examples", "tests", "i18n", "README.md", "Cargo.toml"];

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn files_under(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(path)
            .expect("read dir")
            .map(|entry| entry.expect("dir entry").path())
            .collect();
        entries.sort();
        for entry in entries {
            files_under(&entry, out);
        }
    } else {
        out.push(path.to_path_buf());
    }
}

#[test]
fn no_file_names_the_host_project() {
    let root = crate_root();
    let this = root.join("tests/generic.rs");
    let mut files = Vec::new();
    for name in SCANNED {
        files_under(&root.join(name), &mut files);
    }
    assert!(files.len() > 10, "scanned only {} files", files.len());
    let mut violations = Vec::new();
    for file in files.iter().filter(|file| **file != this) {
        let text = String::from_utf8_lossy(&std::fs::read(file).expect("read file")).into_owned();
        for (index, line) in text.lines().enumerate() {
            if ALLOWED_LINES.contains(&line.trim()) {
                continue;
            }
            let lower = line.to_lowercase();
            for term in FORBIDDEN {
                if lower.contains(term) {
                    violations.push(format!(
                        "{}:{}: {term:?}",
                        file.strip_prefix(&root).unwrap_or(file).display(),
                        index + 1
                    ));
                }
            }
        }
    }
    assert!(violations.is_empty(), "project names in toolkit:\n{}", violations.join("\n"));
}

/// `cargo metadata` at the workspace root with the given extra flags.
fn metadata(extra: &[&str]) -> serde_json::Value {
    let mut command = Command::new(env!("CARGO"));
    command.current_dir(crate_root().join("../.."));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("CARGO_") {
            command.env_remove(key);
        }
    }
    command.args(["metadata", "--format-version", "1", "--locked"]);
    command.args(extra);
    let output = command.output().expect("run cargo metadata");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("metadata JSON")
}

#[test]
fn dependency_closure_reaches_no_workspace_crate_outside_vendor() {
    for extra in [&[][..], &["--all-features"][..]] {
        let meta = metadata(extra);
        let root = PathBuf::from(meta["workspace_root"].as_str().expect("workspace root"));
        let vendor = root.join("vendor");
        let members: HashSet<&str> = meta["workspace_members"]
            .as_array()
            .expect("members")
            .iter()
            .map(|id| id.as_str().expect("member id"))
            .collect();
        let packages: HashMap<&str, &serde_json::Value> = meta["packages"]
            .as_array()
            .expect("packages")
            .iter()
            .map(|package| (package["id"].as_str().expect("package id"), package))
            .collect();
        let nodes: HashMap<&str, &serde_json::Value> = meta["resolve"]["nodes"]
            .as_array()
            .expect("resolve nodes")
            .iter()
            .map(|node| (node["id"].as_str().expect("node id"), node))
            .collect();
        let toolkit = *members
            .iter()
            .find(|id| packages[*id]["name"] == "toolkit")
            .expect("toolkit is a workspace member");
        // Walk normal edges only (a dep_kinds entry with kind null), as
        // `cargo tree -e normal` does.
        let mut seen = HashSet::from([toolkit]);
        let mut queue = VecDeque::from([toolkit]);
        while let Some(id) = queue.pop_front() {
            for dep in nodes[id]["deps"].as_array().expect("deps") {
                let normal = dep["dep_kinds"]
                    .as_array()
                    .expect("dep kinds")
                    .iter()
                    .any(|kind| kind["kind"].is_null());
                let pkg = dep["pkg"].as_str().expect("dep pkg");
                if normal && seen.insert(pkg) {
                    queue.push_back(pkg);
                }
            }
        }
        let mut violations = Vec::new();
        for id in &seen {
            if *id == toolkit {
                continue;
            }
            let package = packages[id];
            let name = package["name"].as_str().expect("name");
            if members.contains(id) {
                violations.push(format!("workspace member {name}"));
            }
            if package["source"].is_null() {
                let manifest = Path::new(package["manifest_path"].as_str().expect("manifest"));
                if !manifest.starts_with(&vendor) {
                    violations.push(format!(
                        "path dependency {name} outside vendor/: {}",
                        manifest.display()
                    ));
                }
            }
        }
        violations.sort();
        assert!(
            violations.is_empty(),
            "toolkit (features {extra:?}) reaches project crates:\n{}",
            violations.join("\n")
        );
        assert!(
            seen.iter().any(|id| packages[id]["name"] == "iced_core"),
            "closure should include the vendored iced family"
        );
    }
}
