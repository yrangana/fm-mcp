//! Checks on the files that ship a release: the plugin, the marketplace,
//! `server.json` and the dist config must agree with each other and with Cargo.toml.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use serde_json::Value;

fn read(path: &str) -> String {
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("{}: {e}", full.display()))
}

fn json(path: &str) -> Value {
    serde_json::from_str(&read(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn cargo() -> toml_edit::DocumentMut {
    read("Cargo.toml").parse().unwrap()
}

#[test]
fn plugin_skill_should_match_the_main_skill() {
    assert!(
        read("plugin/skills/fm-delegate/SKILL.md") == read("skills/fm-delegate/SKILL.md"),
        "plugin/skills/fm-delegate/SKILL.md differs: copy skills/fm-delegate/SKILL.md over it"
    );
}

#[test]
fn every_release_file_should_carry_the_crate_version() {
    let crate_version = cargo()["package"]["version"].as_str().unwrap().to_owned();
    let server = json("server.json");
    let versions = [
        (
            "plugin.json",
            json("plugin/.claude-plugin/plugin.json")["version"].clone(),
        ),
        ("server.json", server["version"].clone()),
        (
            "server.json package",
            server["packages"][0]["version"].clone(),
        ),
    ];
    for (file, version) in versions {
        assert_eq!(version, crate_version.as_str(), "{file} version");
    }
}

/// Hard rule (AGENTS.md): no "Apple" in any product name, for trademark reasons.
#[test]
fn no_published_name_should_contain_apple() {
    let cargo = cargo();
    let marketplace = json(".claude-plugin/marketplace.json");
    let dist: toml_edit::DocumentMut = read("dist-workspace.toml").parse().unwrap();
    let mut names = vec![
        cargo["package"]["name"].as_str().unwrap().to_owned(),
        json("plugin/.claude-plugin/plugin.json")["name"].to_string(),
        marketplace["name"].to_string(),
        json("server.json")["name"].to_string(),
        dist["dist"]["tap"].as_str().unwrap().to_owned(),
    ];
    names.extend(
        cargo["bin"]
            .as_array_of_tables()
            .unwrap()
            .iter()
            .map(|bin| bin["name"].as_str().unwrap().to_owned()),
    );
    names.extend(
        marketplace["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|plugin| plugin["name"].to_string()),
    );
    let bad: Vec<_> = names
        .iter()
        .filter(|name| name.to_lowercase().contains("apple"))
        .collect();
    assert!(bad.is_empty(), "names containing \"apple\": {bad:?}");
}
