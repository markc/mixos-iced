// SPDX-License-Identifier: MIT OR Apache-2.0
//! The direction of the dependency: `appearance` depends on `toolkit`, and
//! toolkit's own closure stays free of every workspace crate, this one
//! included. toolkit's `generic` gate proves the second half for toolkit;
//! this test proves it again from the adapter's side, with the first half.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::Command;

fn metadata() -> serde_json::Value {
    let mut command = Command::new(env!("CARGO"));
    command.current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("CARGO_") {
            command.env_remove(key);
        }
    }
    command.args(["metadata", "--format-version", "1", "--locked"]);
    let output = command.output().expect("run cargo metadata");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("metadata JSON")
}

/// The names of the workspace members in `name`'s normal-dependency
/// closure (`cargo tree -p <name> -e normal`), excluding itself.
fn workspace_closure(meta: &serde_json::Value, name: &str) -> HashSet<String> {
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
    let start = *members
        .iter()
        .find(|id| packages[*id]["name"] == name)
        .unwrap_or_else(|| panic!("{name} is a workspace member"));
    let mut seen = HashSet::from([start]);
    let mut queue = VecDeque::from([start]);
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
    seen.into_iter()
        .filter(|id| *id != start && members.contains(id))
        .map(|id| packages[id]["name"].as_str().expect("name").to_owned())
        .collect()
}

#[test]
fn appearance_depends_on_toolkit_and_never_the_reverse() {
    let meta = metadata();
    let toolkit = workspace_closure(&meta, "toolkit");
    assert!(
        toolkit.is_empty(),
        "toolkit reaches workspace crates: {toolkit:?}"
    );
    let appearance = workspace_closure(&meta, "appearance");
    for required in ["toolkit", "design", "assets", "config"] {
        assert!(appearance.contains(required), "appearance should reach {required}");
    }
    // Not even as a dev-dependency: toolkit's tests never see the adapter.
    let toolkit_manifest = meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .find(|package| package["name"] == "toolkit")
        .expect("toolkit package");
    assert!(
        toolkit_manifest["dependencies"]
            .as_array()
            .expect("dependencies")
            .iter()
            .all(|dep| dep["name"] != "appearance"),
        "toolkit declares appearance"
    );
}
