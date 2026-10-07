//! Claude Code: the MCP server entry in `~/.claude.json`, and the skill.
//!
//! The entry is added with `claude mcp add -s user`, which writes
//! `$CLAUDE_CONFIG_DIR/.claude.json` or `~/.claude.json` (checked 2026-10-08,
//! Claude Code 2.1.293). Without the `claude` command, fm-mcp edits that file
//! directly, keeping its key order and 2-space layout.

use std::{
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::{
    Entry, Kind, Paths, SERVER_NAME,
    files::{self, Action, Change},
};
use crate::guidance::SKILL;

/// The fm-mcp entry in `~/.claude.json`, if any. Fails if the file isn't valid JSON.
pub fn configured_command(paths: &Paths) -> Result<Option<String>> {
    let Some(config) = read_config(&paths.claude_json)? else {
        return Ok(None);
    };
    Ok(config["mcpServers"][SERVER_NAME]["command"]
        .as_str()
        .map(str::to_owned))
}

fn read_config(path: &Path) -> Result<Option<Value>> {
    let Some(text) =
        files::read(path).with_context(|| format!("cannot read {}", path.display()))?
    else {
        return Ok(None);
    };
    let value: Value = serde_json::from_str(&text).with_context(|| {
        format!(
            "{} is not valid JSON, so fm-mcp did not change it. Fix or move the file, then run \
             `fm-mcp install` again",
            path.display()
        )
    })?;
    if !value.is_object() {
        bail!(
            "{} is not a JSON object; fm-mcp did not change it",
            path.display()
        );
    }
    Ok(Some(value))
}

fn is_ours(entry: &Value, binary: &str) -> bool {
    entry["command"] == json!(binary)
        && entry["args"].as_array().is_none_or(Vec::is_empty)
        && matches!(entry["type"].as_str(), None | Some("stdio"))
}

/// Adds the MCP server, or reports it is already there.
pub fn install_server(
    paths: &Paths,
    binary: &str,
    claude: Option<&Path>,
    dry_run: bool,
) -> Result<(Change, Option<Entry>)> {
    let path = &paths.claude_json;
    let config = read_config(path)?;
    let existing = config
        .as_ref()
        .and_then(|c| c["mcpServers"].get(SERVER_NAME));
    let via = if claude.is_some() {
        "MCP server `fm-mcp`, via `claude mcp add -s user`"
    } else {
        "MCP server `fm-mcp`; `claude` not found, so the file was edited directly"
    };
    let mut change = Change {
        path: path.clone(),
        action: Action::Unchanged,
        what: via.into(),
        backup: None,
    };
    if existing.is_some_and(|e| is_ours(e, binary)) {
        change.what = "MCP server `fm-mcp` already configured".into();
        return Ok((change, None));
    }
    change.action = if config.is_some() {
        Action::Updated
    } else {
        Action::Created
    };
    let entry = Entry {
        kind: Kind::ClaudeMcp,
        path: path.clone(),
        created_file: config.is_none(),
        created_table: config
            .as_ref()
            .is_none_or(|c| c.get("mcpServers").is_none()),
        ..Entry::default()
    };
    if dry_run {
        return Ok((change, Some(entry)));
    }
    match claude {
        Some(claude) => {
            if config.is_some() {
                change.backup = Some(files::backup(path)?);
            }
            if existing.is_some() {
                run_claude(claude, &["mcp", "remove", "-s", "user", SERVER_NAME])?;
            }
            run_claude(
                claude,
                &["mcp", "add", "-s", "user", SERVER_NAME, "--", binary],
            )?;
        }
        None => {
            let mut config = config.unwrap_or_else(|| json!({}));
            let servers = config
                .as_object_mut()
                .context("not a JSON object")?
                .entry("mcpServers")
                .or_insert_with(|| json!({}));
            let Some(servers) = servers.as_object_mut() else {
                bail!("`mcpServers` in {} is not an object", path.display());
            };
            servers.insert(
                SERVER_NAME.into(),
                json!({"type": "stdio", "command": binary, "args": [], "env": {}}),
            );
            change.backup = write_config(path, &config)?;
        }
    }
    Ok((change, Some(entry)))
}

/// Removes the MCP server entry, if present.
pub fn uninstall_server(
    paths: &Paths,
    entry: &Entry,
    claude: Option<&Path>,
    dry_run: bool,
) -> Result<Option<Change>> {
    let path = &entry.path;
    let Some(config) = read_config(path)? else {
        return Ok(None);
    };
    if config["mcpServers"].get(SERVER_NAME).is_none() {
        return Ok(None);
    }
    let mut change = Change {
        path: path.clone(),
        action: Action::Updated,
        what: "removed MCP server `fm-mcp`".into(),
        backup: None,
    };
    if dry_run {
        return Ok(Some(change));
    }
    // The CLI only knows the file for the current environment.
    let cli_owns_file = *path == paths.claude_json;
    match claude {
        Some(claude) if cli_owns_file => {
            change.backup = Some(files::backup(path)?);
            run_claude(claude, &["mcp", "remove", "-s", "user", SERVER_NAME])?;
        }
        _ => {
            let mut config = config;
            let Some(object) = config.as_object_mut() else {
                return Ok(None);
            };
            let now_empty = match object.get_mut("mcpServers").and_then(Value::as_object_mut) {
                Some(servers) => {
                    servers.shift_remove(SERVER_NAME);
                    servers.is_empty()
                }
                None => false,
            };
            if now_empty && entry.created_table {
                object.shift_remove("mcpServers");
            }
            if object.is_empty() && entry.created_file {
                change.backup = files::remove(path, true, false)?;
                change.action = Action::Removed;
                change.what = "created by fm-mcp, and now empty".into();
            } else {
                change.backup = write_config(path, &config)?;
            }
        }
    }
    Ok(Some(change))
}

/// Writes JSON the way Claude Code does: 2-space indent, and a final newline
/// only if the file had one. Returns the backup's path.
fn write_config(path: &Path, config: &Value) -> Result<Option<std::path::PathBuf>> {
    let had_newline = files::read(path)?.is_some_and(|t| t.ends_with('\n'));
    let mut text = serde_json::to_string_pretty(config)?;
    if had_newline {
        text.push('\n');
    }
    Ok(files::write(path, &text, false)?.backup)
}

fn run_claude(claude: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new(claude)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("could not run {}", claude.display()))?;
    if !output.status.success() {
        bail!(
            "`claude {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Writes the delegation skill.
pub fn install_skill(paths: &Paths, dry_run: bool) -> Result<(Change, Entry)> {
    let path = paths.skill_file();
    let written = files::write(&path, SKILL, dry_run)
        .with_context(|| format!("cannot write {}", path.display()))?;
    let change = Change {
        path: path.clone(),
        action: written.action,
        what: "skill `fm-delegate`: when to delegate".into(),
        backup: written.backup,
    };
    let entry = Entry {
        kind: Kind::ClaudeSkill,
        path,
        created_file: written.action == Action::Created,
        created_dirs: written.created_dirs,
        ..Entry::default()
    };
    Ok((change, entry))
}

/// Deletes the skill file and the folders fm-mcp created for it.
pub fn uninstall_skill(entry: &Entry, dry_run: bool) -> Result<Option<Change>> {
    if !entry.path.exists() {
        return Ok(None);
    }
    // Back up only if someone has edited it since.
    let edited = files::read(&entry.path)?.as_deref() != Some(SKILL);
    let backup = files::remove(&entry.path, !edited, dry_run)?;
    let mut dirs = entry.created_dirs.clone();
    // The skill's own folder is fm-mcp's even if the manifest is missing.
    if let Some(dir) = entry.path.parent()
        && !dirs.iter().any(|d| d == dir)
    {
        dirs.push(dir.to_path_buf());
    }
    files::remove_empty_dirs(&dirs, dry_run);
    Ok(Some(Change {
        path: entry.path.clone(),
        action: Action::Removed,
        what: "skill `fm-delegate`".into(),
        backup,
    }))
}
