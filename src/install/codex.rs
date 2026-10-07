//! Codex: `[mcp_servers.fm-mcp]` in `config.toml`, and the delegation block
//! in the global AGENTS file Codex actually reads.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use toml_edit::{Array, DocumentMut, Item, Table, value};

use super::{
    Entry, Kind, Paths, SERVER_NAME,
    files::{self, Action, Change},
};
use crate::guidance::AGENTS_SNIPPET;

/// Codex's default of 60 s is too short for a long `summarise`, and requests
/// queue behind other sessions (plan R11).
const TOOL_TIMEOUT_SECS: i64 = 180;

fn read_config(path: &Path) -> Result<Option<(String, DocumentMut)>> {
    let Some(text) =
        files::read(path).with_context(|| format!("cannot read {}", path.display()))?
    else {
        return Ok(None);
    };
    let doc = text.parse::<DocumentMut>().with_context(|| {
        format!(
            "{} is not valid TOML, so fm-mcp did not change it. Fix the file, then run \
             `fm-mcp install` again",
            path.display()
        )
    })?;
    Ok(Some((text, doc)))
}

/// The `command` of `[mcp_servers.fm-mcp]`, if configured.
pub fn configured_command(paths: &Paths) -> Result<Option<String>> {
    let Some((_, doc)) = read_config(&paths.codex_config())? else {
        return Ok(None);
    };
    Ok(doc
        .get("mcp_servers")
        .and_then(|s| s.get(SERVER_NAME))
        .and_then(|s| s.get("command"))
        .and_then(Item::as_str)
        .map(str::to_owned))
}

pub fn install_server(paths: &Paths, binary: &str, dry_run: bool) -> Result<(Change, Entry)> {
    let path = paths.codex_config();
    let (old_text, mut doc) = read_config(&path)?.unwrap_or_default();
    let created_table = doc.get("mcp_servers").is_none();
    if created_table {
        let mut servers = Table::new();
        // Shows as `[mcp_servers.fm-mcp]`, without an empty `[mcp_servers]` header.
        servers.set_implicit(true);
        doc.insert("mcp_servers", Item::Table(servers));
    }
    let Some(servers) = doc.get_mut("mcp_servers").and_then(Item::as_table_mut) else {
        bail!("`mcp_servers` in {} is not a table", path.display());
    };
    let server = servers
        .entry(SERVER_NAME)
        .or_insert_with(|| Item::Table(Table::new()));
    let Some(server) = server.as_table_mut() else {
        bail!(
            "`mcp_servers.{SERVER_NAME}` in {} is not a table",
            path.display()
        );
    };
    if server.get("command").and_then(Item::as_str) != Some(binary) {
        server.insert("command", value(binary));
    }
    if server
        .get("args")
        .and_then(Item::as_array)
        .is_none_or(|a| !a.is_empty())
    {
        server.insert("args", value(Array::new()));
    }
    // Keep a timeout the user chose.
    if server.get("tool_timeout_sec").is_none() {
        server.insert("tool_timeout_sec", value(TOOL_TIMEOUT_SECS));
    }

    let text = doc.to_string();
    let existed = path.exists();
    let written = if text == old_text && existed {
        files::Written {
            action: Action::Unchanged,
            backup: None,
            created_dirs: Vec::new(),
        }
    } else {
        files::write(&path, &text, dry_run)
            .with_context(|| format!("cannot write {}", path.display()))?
    };
    let what = if written.action == Action::Unchanged {
        "MCP server `fm-mcp` already configured"
    } else {
        "MCP server `fm-mcp`, tool timeout 180 s"
    };
    let change = Change {
        path: path.clone(),
        action: written.action,
        what: what.into(),
        backup: written.backup,
    };
    let entry = Entry {
        kind: Kind::CodexMcp,
        path,
        created_file: !existed,
        created_table,
        created_dirs: written.created_dirs,
        ..Entry::default()
    };
    Ok((change, entry))
}

pub fn uninstall_server(entry: &Entry, dry_run: bool) -> Result<Option<Change>> {
    let path = &entry.path;
    let Some((_, mut doc)) = read_config(path)? else {
        return Ok(None);
    };
    let Some(servers) = doc.get_mut("mcp_servers").and_then(Item::as_table_mut) else {
        return Ok(None);
    };
    if servers.remove(SERVER_NAME).is_none() {
        return Ok(None);
    }
    if servers.is_empty() && entry.created_table {
        doc.remove("mcp_servers");
    }
    let text = doc.to_string();
    let (action, backup) = if text.trim().is_empty() && entry.created_file {
        (Action::Removed, files::remove(path, true, dry_run)?)
    } else {
        let written = files::write(path, &text, dry_run)?;
        (written.action, written.backup)
    };
    if action == Action::Removed {
        files::remove_empty_dirs(&entry.created_dirs, dry_run);
    }
    Ok(Some(Change {
        path: path.clone(),
        action,
        what: "removed MCP server `fm-mcp`".into(),
        backup,
    }))
}

/// The global AGENTS file Codex reads: a non-empty `AGENTS.override.md`
/// hides `AGENTS.md` (plan N5). Whitespace-only counts as empty here; the
/// Codex docs don't say (to confirm in Phase 6).
pub fn agents_file(paths: &Paths) -> PathBuf {
    let override_file = paths.codex_home.join("AGENTS.override.md");
    match files::read(&override_file) {
        Ok(Some(text)) if !text.trim().is_empty() => override_file,
        _ => paths.codex_home.join("AGENTS.md"),
    }
}

/// The other global AGENTS file, which Codex ignores while `agents_file` is in use.
pub fn hidden_agents_file(paths: &Paths) -> PathBuf {
    let read = agents_file(paths);
    if read.ends_with("AGENTS.md") {
        paths.codex_home.join("AGENTS.override.md")
    } else {
        paths.codex_home.join("AGENTS.md")
    }
}

/// True if the fm-mcp block is in the file Codex reads.
pub fn snippet_installed(paths: &Paths) -> bool {
    files::read(&agents_file(paths))
        .ok()
        .flatten()
        .and_then(|text| files::find_block(&text))
        .is_some()
}

pub fn install_snippet(paths: &Paths, dry_run: bool) -> Result<(Change, Entry)> {
    let path = agents_file(paths);
    let old = files::read(&path)?.unwrap_or_default();
    let (text, separator) = files::insert_block(&old, AGENTS_SNIPPET);
    let written = files::write(&path, &text, dry_run)
        .with_context(|| format!("cannot write {}", path.display()))?;
    let change = Change {
        path: path.clone(),
        action: written.action,
        what: "fm-mcp delegation rules, between the fm-mcp:begin and fm-mcp:end markers".into(),
        backup: written.backup,
    };
    let entry = Entry {
        kind: Kind::CodexSnippet,
        path,
        created_file: written.action == Action::Created,
        separator,
        created_dirs: written.created_dirs,
        ..Entry::default()
    };
    Ok((change, entry))
}

pub fn uninstall_snippet(entry: &Entry, dry_run: bool) -> Result<Option<Change>> {
    let path = &entry.path;
    let Some(old) = files::read(path)? else {
        return Ok(None);
    };
    let Some(text) = files::remove_block(&old, &entry.separator) else {
        return Ok(None);
    };
    let (action, backup) = if text.trim().is_empty() && entry.created_file {
        let backup = files::remove(path, true, dry_run)?;
        files::remove_empty_dirs(&entry.created_dirs, dry_run);
        (Action::Removed, backup)
    } else {
        let written = files::write(path, &text, dry_run)?;
        (written.action, written.backup)
    };
    Ok(Some(Change {
        path: path.clone(),
        action,
        what: "removed the fm-mcp delegation rules".into(),
        backup,
    }))
}
