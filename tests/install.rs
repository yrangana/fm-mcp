//! `fm-mcp install` and `fm-mcp doctor`, run as the real binary against a
//! scratch home folder. `PATH` is limited so the real `claude` and `codex`
//! are never found; a test that needs `claude` puts a fake on `PATH`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use common::fake_fm_path;
use tempfile::TempDir;

struct Home {
    dir: TempDir,
}

struct Run {
    ok: bool,
    stdout: String,
    stderr: String,
}

impl Home {
    /// A home folder with `~/Library`, like a real Mac.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("home/Library")).unwrap();
        fs::create_dir_all(dir.path().join("bin")).unwrap();
        Self { dir }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.home().join(relative)
    }

    fn write(&self, relative: &str, content: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn read(&self, relative: &str) -> Option<String> {
        fs::read_to_string(self.path(relative)).ok()
    }

    /// Puts a fake `claude` on `PATH`. It logs its arguments and writes
    /// `~/.claude.json` the way `claude mcp add -s user` does.
    fn fake_claude(&self) {
        let script = format!(
            r#"#!/bin/sh
echo "$@" >> "{log}"
if [ "$2" = add ]; then
  for last; do :; done
  printf '{{\n  "mcpServers": {{\n    "fm-mcp": {{\n      "type": "stdio",\n      "command": "%s",\n      "args": [],\n      "env": {{}}\n    }}\n  }}\n}}' "$last" > "$HOME/.claude.json"
fi
"#,
            log = self.dir.path().join("claude.log").display()
        );
        let path = self.dir.path().join("bin/claude");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn claude_log(&self) -> String {
        fs::read_to_string(self.dir.path().join("claude.log")).unwrap_or_default()
    }

    fn run(&self, args: &[&str]) -> Run {
        self.run_with(args, &[])
    }

    fn run_with(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let path = format!("{}:/usr/bin:/bin", self.dir.path().join("bin").display());
        let mut command = Command::new(env!("CARGO_BIN_EXE_fm-mcp"));
        command
            .args(args)
            .env("HOME", self.home())
            .env("PATH", path)
            .env("FM_MCP_FM_PATH", fake_fm_path())
            .env("FM_MCP_LOG", "warn")
            .env_remove("CODEX_HOME")
            .env_remove("CLAUDE_CONFIG_DIR");
        for (key, value) in env {
            command.env(key, value);
        }
        let Output {
            status,
            stdout,
            stderr,
        } = command.output().unwrap();
        Run {
            ok: status.success(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        }
    }

    /// Every file under the home folder and its bytes.
    fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, base, out);
                } else {
                    let relative = path.strip_prefix(base).unwrap().to_path_buf();
                    out.insert(relative, fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.home(), &self.home(), &mut out);
        out
    }

    /// Like `files`, without fm-mcp's backups.
    fn files_without_backups(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = self.files();
        files.retain(|path, _| !path.to_string_lossy().contains(".fm-mcp-backup-"));
        files
    }

    fn backups(&self) -> usize {
        self.files().len() - self.files_without_backups().len()
    }
}

fn binary() -> String {
    Path::new(env!("CARGO_BIN_EXE_fm-mcp"))
        .canonicalize()
        .unwrap()
        .display()
        .to_string()
}

/// `~/.claude.json` as Claude Code writes it: 2-space indent, no final newline.
const CLAUDE_JSON: &str = r#"{
  "numStartups": 3,
  "mcpServers": {
    "context7": {
      "type": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "@upstash/context7-mcp"
      ],
      "env": {}
    }
  },
  "projects": {
    "/Users/me/app": {
      "allowedTools": []
    }
  }
}"#;

const CODEX_CONFIG: &str = r#"# My Codex settings
model = "gpt-5-codex"

[mcp_servers.docs]
command = "docs-mcp" # keep this comment
args = ["--port", "1"]
"#;

const AGENTS: &str = "# My global rules\n\nBe brief.\n";

/// A home with existing Claude Code and Codex settings.
fn configured_home() -> Home {
    let home = Home::new();
    home.write(".claude.json", CLAUDE_JSON);
    home.write(".codex/config.toml", CODEX_CONFIG);
    home.write(".codex/AGENTS.md", AGENTS);
    home
}

#[test]
fn install_should_add_fm_mcp_and_keep_other_servers() {
    let home = configured_home();
    let run = home.run(&["install"]);
    let claude: serde_json::Value =
        serde_json::from_str(&home.read(".claude.json").unwrap()).unwrap();
    let codex = home.read(".codex/config.toml").unwrap();
    assert!(run.ok, "{}{}", run.stdout, run.stderr);
    assert_eq!(
        (
            claude["mcpServers"]["fm-mcp"]["command"].as_str(),
            claude["mcpServers"]["context7"]["command"].as_str(),
        ),
        (Some(binary().as_str()), Some("npx"))
    );
    assert!(
        codex.starts_with(CODEX_CONFIG)
            && codex.contains(&format!(
                "[mcp_servers.fm-mcp]\ncommand = \"{}\"\nargs = []\ntool_timeout_sec = 180\n",
                binary()
            )),
        "{codex}"
    );
}

#[test]
fn install_should_write_the_skill_and_the_codex_rules() {
    let home = configured_home();
    home.run(&["install"]);
    let agents = home.read(".codex/AGENTS.md").unwrap();
    assert!(
        home.read(".claude/skills/fm-delegate/SKILL.md")
            .unwrap()
            .contains("name: fm-delegate")
            && agents.starts_with(&format!("{AGENTS}\n<!-- fm-mcp:begin"))
            && agents.ends_with("<!-- fm-mcp:end -->\n"),
        "{agents}"
    );
}

#[test]
fn install_should_print_every_path_it_changed() {
    let home = configured_home();
    let run = home.run(&["install"]);
    for file in [
        ".claude.json",
        ".claude/skills/fm-delegate/SKILL.md",
        ".codex/config.toml",
        ".codex/AGENTS.md",
    ] {
        assert!(
            run.stdout.contains(&home.path(file).display().to_string()),
            "{file} missing from:\n{}",
            run.stdout
        );
    }
}

#[test]
fn install_twice_should_change_nothing_the_second_time() {
    let home = configured_home();
    home.run(&["install"]);
    let after_first = home.files();
    let second = home.run(&["install"]);
    assert!(
        home.files() == after_first && second.stdout.contains("Already configured"),
        "{}",
        second.stdout
    );
}

#[test]
fn install_should_back_up_each_existing_file_once() {
    let home = configured_home();
    home.run(&["install"]);
    home.run(&["install"]);
    assert_eq!(home.backups(), 3);
}

const SKILL: &str = include_str!("../skills/fm-delegate/SKILL.md");
const SKILL_FILE: &str = ".claude/skills/fm-delegate/SKILL.md";
const MANIFEST: &str = "Library/Application Support/fm-mcp/install.json";

/// Puts an "older fm-mcp" skill in place, recorded in the manifest as fm-mcp's own.
fn install_an_older_skill(home: &Home, text: &str) {
    home.write(SKILL_FILE, text);
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    let mut manifest: serde_json::Value =
        serde_json::from_str(&home.read(MANIFEST).unwrap()).unwrap();
    for entry in manifest["entries"].as_array_mut().unwrap() {
        if entry["kind"] == "claude_skill" {
            entry["written"] = format!("fnv1a64:{hash:016x}").into();
        }
    }
    home.write(MANIFEST, &manifest.to_string());
}

#[test]
fn install_should_replace_its_own_older_skill_without_a_backup() {
    let home = configured_home();
    home.run(&["install", "--claude"]);
    install_an_older_skill(&home, "fm-mcp's skill, an older version\n");
    let backups = home.backups();
    home.run(&["install", "--claude"]);
    assert_eq!(
        (home.read(SKILL_FILE).as_deref(), home.backups()),
        (Some(SKILL), backups)
    );
}

#[test]
fn install_should_back_up_a_skill_someone_edited() {
    let home = configured_home();
    home.run(&["install", "--claude"]);
    home.write(SKILL_FILE, "my own edits\n");
    let backups = home.backups();
    home.run(&["install", "--claude"]);
    assert_eq!(home.backups(), backups + 1);
}

#[test]
fn uninstall_should_not_back_up_its_own_older_skill() {
    let home = configured_home();
    home.run(&["install", "--claude"]);
    install_an_older_skill(&home, "fm-mcp's skill, an older version\n");
    home.run(&["install", "--uninstall", "--claude"]);
    // A backup would keep the skill's folder from being removed.
    assert!(!home.path(".claude/skills/fm-delegate").exists());
}

#[test]
fn uninstall_should_restore_every_file_exactly() {
    let home = configured_home();
    let before = home.files();
    home.run(&["install"]);
    let run = home.run(&["install", "--uninstall"]);
    assert!(run.ok, "{}{}", run.stdout, run.stderr);
    assert_eq!(home.files_without_backups(), before);
}

#[test]
fn uninstall_should_remove_everything_from_a_fresh_home() {
    let home = Home::new();
    home.run(&["install", "--claude", "--codex"]);
    home.run(&["install", "--uninstall"]);
    assert_eq!(
        (
            home.files().len(),
            home.path(".claude").exists(),
            home.path(".codex").exists()
        ),
        (0, false, false)
    );
}

#[test]
fn uninstall_without_a_record_should_still_remove_fm_mcp() {
    let home = configured_home();
    let before = home.files();
    home.run(&["install"]);
    fs::remove_file(home.path("Library/Application Support/fm-mcp/install.json")).unwrap();
    home.run(&["install", "--uninstall"]);
    let files = home.files_without_backups();
    assert_eq!(
        files.keys().collect::<Vec<_>>(),
        before.keys().collect::<Vec<_>>()
    );
    assert!(!String::from_utf8_lossy(&files[Path::new(".codex/config.toml")]).contains("fm-mcp"));
}

#[test]
fn dry_run_should_write_nothing() {
    let home = configured_home();
    let before = home.files();
    let run = home.run(&["install", "--dry-run"]);
    assert!(
        home.files() == before && run.stdout.contains("would update"),
        "{}",
        run.stdout
    );
}

#[test]
fn no_guidance_should_skip_the_skill_and_the_rules() {
    let home = configured_home();
    home.run(&["install", "--no-guidance"]);
    assert_eq!(
        (
            home.path(".claude/skills").exists(),
            home.read(".codex/AGENTS.md").as_deref()
        ),
        (false, Some(AGENTS))
    );
}

#[test]
fn install_should_refuse_a_malformed_codex_config_and_write_nothing() {
    let home = configured_home();
    home.write(".codex/config.toml", "[mcp_servers\ncommand = ");
    let before = home.files();
    let run = home.run(&["install"]);
    assert!(
        !run.ok
            && run
                .stderr
                .contains(&home.path(".codex/config.toml").display().to_string())
            && home.files() == before,
        "{}",
        run.stderr
    );
}

#[test]
fn install_should_refuse_a_malformed_claude_json_and_write_nothing() {
    let home = configured_home();
    home.write(".claude.json", "{ not json");
    let before = home.files();
    let run = home.run(&["install"]);
    assert!(!run.ok && home.files() == before, "{}", run.stderr);
}

#[test]
fn install_should_keep_a_codex_timeout_the_user_chose() {
    let home = Home::new();
    home.write(
        ".codex/config.toml",
        "[mcp_servers.fm-mcp]\ncommand = \"old\"\ntool_timeout_sec = 600\n",
    );
    home.run(&["install", "--codex"]);
    let config = home.read(".codex/config.toml").unwrap();
    assert!(
        config.contains("tool_timeout_sec = 600") && config.contains(&binary()),
        "{config}"
    );
}

#[test]
fn install_without_agents_should_explain_what_to_do() {
    let home = Home::new();
    let run = home.run(&["install"]);
    assert!(
        !run.ok && run.stderr.contains("found neither Claude Code nor Codex"),
        "{}",
        run.stderr
    );
}

#[test]
fn install_should_use_the_claude_command_when_available() {
    let home = Home::new();
    home.fake_claude();
    let run = home.run(&["install", "--claude"]);
    assert_eq!(
        home.claude_log(),
        format!("mcp add -s user fm-mcp -- {}\n", binary()),
        "{}{}",
        run.stdout,
        run.stderr
    );
}

#[test]
fn install_should_replace_a_stale_claude_entry_through_the_claude_command() {
    let home = Home::new();
    home.fake_claude();
    home.write(
        ".claude.json",
        r#"{"mcpServers": {"fm-mcp": {"command": "/old/fm-mcp", "args": []}}}"#,
    );
    home.run(&["install", "--claude"]);
    assert_eq!(
        home.claude_log(),
        format!(
            "mcp remove -s user fm-mcp\nmcp add -s user fm-mcp -- {}\n",
            binary()
        )
    );
}

#[test]
fn install_should_skip_the_claude_command_when_already_configured() {
    let home = Home::new();
    home.fake_claude();
    home.run(&["install", "--claude"]);
    home.run(&["install", "--claude"]);
    assert_eq!(home.claude_log().lines().count(), 1);
}

#[test]
fn uninstall_should_use_the_claude_command_when_available() {
    let home = Home::new();
    home.fake_claude();
    home.run(&["install", "--claude"]);
    home.run(&["install", "--uninstall"]);
    assert!(
        home.claude_log().ends_with("mcp remove -s user fm-mcp\n"),
        "{}",
        home.claude_log()
    );
}

// Codex AGENTS files: the block must go where Codex reads it (plan N5).

fn doctor_line(home: &Home, env: &[(&str, &str)], name: &str) -> String {
    let run = home.run_with(&["doctor"], env);
    run.stdout
        .lines()
        .find(|l| l[l.char_indices().nth(2).map_or(0, |(i, _)| i)..].starts_with(name))
        .unwrap_or_else(|| panic!("no {name} line in:\n{}", run.stdout))
        .to_owned()
}

#[test]
fn codex_rules_should_go_into_agents_md_without_an_override() {
    let home = Home::new();
    home.run(&["install", "--codex"]);
    assert!(
        home.read(".codex/AGENTS.md")
            .unwrap()
            .contains("fm-mcp:begin")
            && doctor_line(&home, &[], "Codex guidance").starts_with('✓')
    );
}

#[test]
fn codex_rules_should_go_into_a_non_empty_override() {
    let home = Home::new();
    home.write(".codex/AGENTS.md", AGENTS);
    home.write(".codex/AGENTS.override.md", "# Override\n");
    home.run(&["install", "--codex"]);
    assert!(
        home.read(".codex/AGENTS.override.md")
            .unwrap()
            .contains("fm-mcp:begin")
            && home.read(".codex/AGENTS.md").as_deref() == Some(AGENTS)
            && doctor_line(&home, &[], "Codex guidance").starts_with('✓')
    );
}

#[test]
fn codex_rules_should_skip_an_empty_override() {
    let home = Home::new();
    home.write(".codex/AGENTS.override.md", "");
    home.run(&["install", "--codex"]);
    assert!(
        home.read(".codex/AGENTS.md")
            .unwrap()
            .contains("fm-mcp:begin")
            && home.read(".codex/AGENTS.override.md").as_deref() == Some("")
            && doctor_line(&home, &[], "Codex guidance").starts_with('✓')
    );
}

#[test]
fn codex_should_honour_codex_home() {
    let home = Home::new();
    let codex_home = home.path("elsewhere/codex");
    let env = [("CODEX_HOME", codex_home.to_str().unwrap())];
    home.run_with(&["install", "--codex"], &env);
    assert!(
        codex_home.join("config.toml").is_file()
            && codex_home.join("AGENTS.md").is_file()
            && !home.path(".codex").exists()
            && doctor_line(&home, &env, "Codex guidance").starts_with('✓')
    );
}

#[test]
fn doctor_should_flag_rules_hidden_by_a_newer_override() {
    let home = Home::new();
    home.run(&["install", "--codex"]);
    home.write(".codex/AGENTS.override.md", "# Override\n");
    let line = doctor_line(&home, &[], "Codex guidance");
    assert!(
        line.starts_with('✗') && line.contains("but Codex reads"),
        "{line}"
    );
}

// doctor

#[test]
fn doctor_should_explain_how_to_turn_on_the_model_and_fail() {
    let home = Home::new();
    let run = home.run_with(&["doctor"], &[("FAKE_FM_AVAILABLE", "false")]);
    assert!(
        !run.ok && run.stdout.contains("turn on Apple Intelligence"),
        "{}",
        run.stdout
    );
}

#[test]
fn doctor_should_explain_how_to_agree_to_the_licence() {
    let home = Home::new();
    let line = doctor_line(&home, &[("FAKE_FM_LICENSE", "false")], "`fm` licence");
    assert!(line.starts_with('✗'), "{line}");
}

#[test]
fn doctor_should_run_a_model_test_through_fm_serve() {
    let home = Home::new();
    let line = doctor_line(&home, &[], "Model test");
    assert!(line.starts_with('✓'), "{line}");
}

#[test]
fn doctor_should_pass_the_agent_checks_after_install() {
    let home = configured_home();
    home.run(&["install"]);
    let run = home.run(&["doctor"]);
    for name in [
        "Claude Code config",
        "Codex config",
        "Claude Code skill",
        "Codex guidance",
        "Configured binary",
    ] {
        assert!(
            run.stdout
                .lines()
                .any(|l| l.starts_with('✓') && l.contains(name)),
            "{name} not passing in:\n{}",
            run.stdout
        );
    }
}

#[test]
fn doctor_should_flag_an_agent_that_is_not_configured() {
    let home = configured_home();
    let line = doctor_line(&home, &[], "Codex config");
    assert!(
        line.starts_with('✗') && line.contains("has no fm-mcp entry"),
        "{line}"
    );
}

#[test]
fn doctor_should_skip_agents_that_are_not_installed() {
    let home = Home::new();
    let line = doctor_line(&home, &[], "Claude Code config");
    assert!(line.starts_with('–'), "{line}");
}
