//! `fm-mcp install`: configures Claude Code and Codex, and writes the
//! delegation guidance. Every change is backed up, recorded in a manifest,
//! and reversed by `fm-mcp install --uninstall`.

mod claude;
mod codex;
mod files;

use std::{
    env,
    ffi::OsString,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use files::{Action, Change};

pub const SERVER_NAME: &str = "fm-mcp";

/// Where each agent keeps its settings, honouring `CLAUDE_CONFIG_DIR` and `CODEX_HOME`.
#[derive(Debug, Clone)]
pub struct Paths {
    pub claude_dir: PathBuf,
    pub claude_json: PathBuf,
    pub codex_home: PathBuf,
    pub manifest: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Self> {
        let home = non_empty_var("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?;
        let claude_config = non_empty_var("CLAUDE_CONFIG_DIR").map(PathBuf::from);
        Ok(Self {
            claude_dir: claude_config
                .clone()
                .unwrap_or_else(|| home.join(".claude")),
            claude_json: claude_config
                .unwrap_or_else(|| home.clone())
                .join(".claude.json"),
            codex_home: non_empty_var("CODEX_HOME")
                .map_or_else(|| home.join(".codex"), PathBuf::from),
            manifest: home.join("Library/Application Support/fm-mcp/install.json"),
        })
    }

    pub fn skill_file(&self) -> PathBuf {
        self.claude_dir.join("skills/fm-delegate/SKILL.md")
    }

    pub fn codex_config(&self) -> PathBuf {
        self.codex_home.join("config.toml")
    }

    pub fn claude_detected(&self) -> bool {
        find_on_path("claude").is_some() || self.claude_json.exists() || self.claude_dir.exists()
    }

    pub fn codex_detected(&self) -> bool {
        find_on_path("codex").is_some() || self.codex_home.exists()
    }
}

fn non_empty_var(name: &str) -> Option<OsString> {
    env::var_os(name).filter(|v| !v.is_empty())
}

/// An executable called `name` on `PATH`.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| {
            path.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// What `install` should do.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    pub claude: bool,
    pub codex: bool,
    pub dry_run: bool,
    pub uninstall: bool,
    pub no_guidance: bool,
}

/// One change recorded in the manifest, with what existed before it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub kind: Kind,
    pub path: PathBuf,
    /// The file didn't exist before fm-mcp wrote it.
    #[serde(default)]
    pub created_file: bool,
    /// `mcpServers` (Claude) or `mcp_servers` (Codex) didn't exist before.
    #[serde(default)]
    pub created_table: bool,
    /// Text added before the AGENTS block, removed with it.
    #[serde(default)]
    pub separator: String,
    /// Folders fm-mcp created, outermost first.
    #[serde(default)]
    pub created_dirs: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    ClaudeMcp,
    ClaudeSkill,
    CodexMcp,
    CodexSnippet,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    binary: String,
    entries: Vec<Entry>,
}

impl Manifest {
    fn load(path: &Path) -> Result<Option<Self>> {
        let Some(text) = files::read(path)? else {
            return Ok(None);
        };
        serde_json::from_str(&text)
            .map(Some)
            .with_context(|| format!("cannot read the install record {}", path.display()))
    }

    /// Keeps the first record for each file: it describes the state before
    /// fm-mcp first touched it.
    fn add(&mut self, entry: Entry) {
        if !self
            .entries
            .iter()
            .any(|e| e.kind == entry.kind && e.path == entry.path)
        {
            self.entries.push(entry);
        }
    }
}

/// A section of the report: one agent and its changes.
struct Section {
    title: &'static str,
    changes: Vec<Change>,
    notes: Vec<String>,
}

pub fn run(options: Options) -> Result<()> {
    let paths = Paths::from_env()?;
    let (claude, codex) = match (options.claude, options.codex) {
        (false, false) => (paths.claude_detected(), paths.codex_detected()),
        chosen => chosen,
    };
    if options.uninstall {
        return uninstall(&paths, claude, codex, options);
    }
    if !claude && !codex {
        bail!(
            "found neither Claude Code nor Codex. Install one, or pass `--claude` or `--codex` \
             to configure it anyway"
        );
    }
    // Check every config parses before changing any, so a broken file means
    // nothing is written.
    if claude {
        claude::configured_command(&paths)?;
    }
    if codex {
        codex::configured_command(&paths)?;
    }
    let (binary, warning) = installed_binary()?;
    let binary_text = binary
        .to_str()
        .context("the fm-mcp path is not valid UTF-8")?
        .to_owned();

    let mut manifest = Manifest::load(&paths.manifest)?.unwrap_or_default();
    let mut sections = Vec::new();
    if claude {
        let mut section = Section {
            title: "Claude Code",
            changes: Vec::new(),
            notes: Vec::new(),
        };
        let cli = find_on_path("claude");
        let (change, entry) =
            claude::install_server(&paths, &binary_text, cli.as_deref(), options.dry_run)?;
        section.changes.push(change);
        entry.into_iter().for_each(|e| manifest.add(e));
        if !options.no_guidance {
            let (change, entry) = claude::install_skill(&paths, options.dry_run)?;
            section.changes.push(change);
            manifest.add(entry);
        }
        if !options.dry_run && cli.is_some() {
            let configured = claude::configured_command(&paths)?;
            if configured.as_deref() != Some(binary_text.as_str()) {
                section.notes.push(format!(
                    "`claude mcp add` ran, but {} doesn't show fm-mcp. If you set \
                     CLAUDE_CONFIG_DIR for Claude Code, set it the same way here.",
                    paths.claude_json.display()
                ));
            }
        }
        sections.push(section);
    }
    if codex {
        let mut section = Section {
            title: "Codex",
            changes: Vec::new(),
            notes: Vec::new(),
        };
        let (change, entry) = codex::install_server(&paths, &binary_text, options.dry_run)?;
        section.changes.push(change);
        manifest.add(entry);
        if !options.no_guidance {
            let (change, entry) = codex::install_snippet(&paths, options.dry_run)?;
            section.changes.push(change);
            manifest.add(entry);
            let hidden = codex::hidden_agents_file(&paths);
            if files::read(&hidden)?
                .as_deref()
                .and_then(files::find_block)
                .is_some()
            {
                section.notes.push(format!(
                    "{} also has fm-mcp rules, but Codex doesn't read that file now. You can \
                     delete that block.",
                    hidden.display()
                ));
            }
        }
        sections.push(section);
    }

    println!(
        "fm-mcp install{}: {}",
        if options.dry_run { " (dry run)" } else { "" },
        binary.display()
    );
    if let Some(warning) = warning {
        println!("warning: {warning}");
    }
    print_sections(&sections, options.dry_run);

    let changed = sections
        .iter()
        .flat_map(|s| &s.changes)
        .any(|c| c.action != Action::Unchanged);
    if options.dry_run {
        println!("\nNothing was written (dry run).");
        return Ok(());
    }
    manifest.version = 1;
    manifest.binary = binary_text;
    let text = serde_json::to_string_pretty(&manifest)? + "\n";
    let written = files::write(&paths.manifest, &text, false)
        .with_context(|| format!("cannot write {}", paths.manifest.display()))?;
    if written.action != Action::Unchanged {
        println!("\nRecord of changes: {}", paths.manifest.display());
    }
    if changed {
        println!(
            "\nStart a new Claude Code or Codex session to use fm-mcp. `fm-mcp doctor` checks \
             the setup; `fm-mcp install --uninstall` reverses it."
        );
    } else {
        println!("\nAlready configured; nothing changed.");
    }
    Ok(())
}

fn uninstall(paths: &Paths, claude: bool, codex: bool, options: Options) -> Result<()> {
    let manifest = Manifest::load(&paths.manifest)?;
    let entries = match &manifest {
        Some(m) => m.entries.clone(),
        // No record: remove what fm-mcp would have added, keeping every file.
        None => vec![
            Entry {
                kind: Kind::ClaudeMcp,
                path: paths.claude_json.clone(),
                ..Entry::default()
            },
            Entry {
                kind: Kind::ClaudeSkill,
                path: paths.skill_file(),
                ..Entry::default()
            },
            Entry {
                kind: Kind::CodexMcp,
                path: paths.codex_config(),
                ..Entry::default()
            },
            Entry {
                kind: Kind::CodexSnippet,
                path: paths.codex_home.join("AGENTS.md"),
                ..Entry::default()
            },
            Entry {
                kind: Kind::CodexSnippet,
                path: paths.codex_home.join("AGENTS.override.md"),
                ..Entry::default()
            },
        ],
    };
    // With no flags, undo everything that was recorded.
    let (claude, codex) = if options.claude || options.codex {
        (claude, codex)
    } else {
        (true, true)
    };
    let cli = find_on_path("claude");
    let mut sections = vec![
        Section {
            title: "Claude Code",
            changes: Vec::new(),
            notes: Vec::new(),
        },
        Section {
            title: "Codex",
            changes: Vec::new(),
            notes: Vec::new(),
        },
    ];
    let mut kept = Vec::new();
    for entry in entries.iter().rev() {
        let change = match entry.kind {
            Kind::ClaudeMcp if claude => {
                claude::uninstall_server(paths, entry, cli.as_deref(), options.dry_run)?
            }
            Kind::ClaudeSkill if claude => claude::uninstall_skill(entry, options.dry_run)?,
            Kind::CodexMcp if codex => codex::uninstall_server(entry, options.dry_run)?,
            Kind::CodexSnippet if codex => codex::uninstall_snippet(entry, options.dry_run)?,
            _ => {
                kept.push(entry.clone());
                continue;
            }
        };
        let section = match entry.kind {
            Kind::ClaudeMcp | Kind::ClaudeSkill => &mut sections[0],
            Kind::CodexMcp | Kind::CodexSnippet => &mut sections[1],
        };
        section.changes.extend(change);
    }
    kept.reverse();

    // Folders fm-mcp created, deepest first, once everything in them is gone.
    let mut dirs: Vec<PathBuf> = entries
        .iter()
        .filter(|e| !kept.contains(e))
        .flat_map(|e| e.created_dirs.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    // Outermost first; `remove_empty_dirs` works from the end.
    dirs.sort_by_key(|d| d.components().count());
    files::remove_empty_dirs(&dirs, options.dry_run);

    println!(
        "fm-mcp install --uninstall{}",
        if options.dry_run { " (dry run)" } else { "" }
    );
    sections.retain(|s| !s.changes.is_empty());
    if sections.is_empty() {
        println!("Nothing to remove: fm-mcp is not configured.");
    }
    print_sections(&sections, options.dry_run);
    if options.dry_run {
        println!("\nNothing was written (dry run).");
        return Ok(());
    }
    match manifest {
        Some(mut manifest) if !kept.is_empty() => {
            manifest.entries = kept;
            let text = serde_json::to_string_pretty(&manifest)? + "\n";
            files::write(&paths.manifest, &text, false)?;
        }
        Some(_) => {
            std::fs::remove_file(&paths.manifest)?;
            if let Some(dir) = paths.manifest.parent() {
                let _ = std::fs::remove_dir(dir);
            }
        }
        None => {}
    }
    Ok(())
}

fn print_sections(sections: &[Section], dry_run: bool) {
    for section in sections {
        println!("\n{}", section.title);
        for change in &section.changes {
            println!(
                "  {:<13} {}  ({})",
                change.action.label(dry_run),
                change.path.display(),
                change.what
            );
            if let Some(backup) = &change.backup {
                println!("  {:<13} {}", "backup", backup.display());
            }
        }
        for note in &section.notes {
            println!("  note: {note}");
        }
    }
}

/// The path agents should run. A Homebrew install runs from a versioned
/// Cellar folder that changes on upgrade, so the stable `bin` link is used
/// instead when it points at the same binary.
pub fn installed_binary() -> Result<(PathBuf, Option<String>)> {
    let exe = env::current_exe().context("cannot find the fm-mcp binary")?;
    let canonical = exe.canonicalize().unwrap_or(exe);
    let text = canonical.to_string_lossy().into_owned();
    if let Some(at) = text.find("/Cellar/") {
        let link = Path::new(&text[..at]).join("bin/fm-mcp");
        if link.canonicalize().ok().as_ref() == Some(&canonical) {
            return Ok((link, None));
        }
        return Ok((
            canonical,
            Some(format!(
                "{text} is inside a versioned Homebrew folder and will move on upgrade; \
                 run `fm-mcp install` again after upgrading"
            )),
        ));
    }
    Ok((canonical, None))
}

/// For `doctor`: the commands each agent is configured to run.
pub struct Configured {
    pub claude: Result<Option<String>>,
    pub codex: Result<Option<String>>,
}

pub fn configured(paths: &Paths) -> Configured {
    Configured {
        claude: claude::configured_command(paths),
        codex: codex::configured_command(paths),
    }
}

pub use codex::{agents_file, hidden_agents_file, snippet_installed};
