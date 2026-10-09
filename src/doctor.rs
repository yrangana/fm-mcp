//! `fm-mcp doctor`: checks everything fm-mcp needs and says how to fix what's missing.

use std::{
    path::Path,
    process::{Command, Stdio},
    time::Instant,
};

use crate::{
    backend::{Backend, ChatMessage, ChatRequest},
    fm::{self, FmConfig, FmServe},
    install::{self, Paths},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Fail,
    /// A problem that doesn't stop fm-mcp working.
    Warn,
    /// Not applicable, e.g. an agent that isn't installed.
    Skip,
}

struct Check {
    name: &'static str,
    status: Status,
    detail: String,
    fix: Option<String>,
}

impl Check {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Pass,
            detail: detail.into(),
            fix: None,
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Fail,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            status: Status::Warn,
            ..Self::fail(name, detail, fix)
        }
    }

    fn skip(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status: Status::Skip,
            ..Self::pass(name, detail)
        }
    }

    fn print(&self) {
        let mark = match self.status {
            Status::Pass => "✓",
            Status::Fail => "✗",
            Status::Warn => "!",
            Status::Skip => "–",
        };
        println!("{mark} {}: {}", self.name, self.detail);
        if let Some(fix) = &self.fix {
            println!("    fix: {fix}");
        }
    }
}

/// Runs every check, printing each as it finishes. True if none failed.
pub async fn run() -> bool {
    let config = FmConfig::from_env();
    let mut failed = false;
    let mut report = |check: Check| {
        check.print();
        failed |= check.status == Status::Fail;
        check.status
    };

    report(apple_silicon());
    report(macos_version());
    let fm_ok = report(fm_binary(&config.fm_path)) == Status::Pass;
    let licence_ok = fm_ok && report(licence(&config.fm_path)) == Status::Pass;
    let available_ok = fm_ok && report(model_available(&config.fm_path)) == Status::Pass;
    report(socket_path());
    if fm_ok && licence_ok && available_ok {
        report(model_answers(config).await);
    } else {
        report(Check::skip(
            "Model test",
            "skipped until the checks above pass",
        ));
    }

    match Paths::from_env() {
        Ok(paths) => {
            for check in agent_checks(&paths) {
                report(check);
            }
        }
        Err(e) => {
            report(Check::fail(
                "Agent config",
                format!("{e:#}"),
                "set HOME and run `fm-mcp doctor` again",
            ));
        }
    }

    println!();
    if failed {
        println!(
            "Some checks failed. Fix them in order; later checks often depend on earlier ones."
        );
    } else {
        println!("All checks passed.");
    }
    !failed
}

fn apple_silicon() -> Check {
    let arch = std::env::consts::ARCH;
    if arch == "aarch64" {
        Check::pass("Apple Silicon", "arm64")
    } else {
        Check::fail(
            "Apple Silicon",
            format!("this fm-mcp is built for {arch}"),
            "fm-mcp needs a Mac with Apple Silicon (M1 or later)",
        )
    }
}

fn macos_version() -> Check {
    let output = Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output();
    let version = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_owned(),
        _ => {
            return Check::fail(
                "macOS 27 or later",
                "could not read the macOS version",
                "fm-mcp needs macOS 27 or later",
            );
        }
    };
    let major: u32 = version
        .split('.')
        .next()
        .and_then(|m| m.parse().ok())
        .unwrap_or(0);
    if major >= 27 {
        Check::pass("macOS 27 or later", version)
    } else {
        Check::fail(
            "macOS 27 or later",
            format!("this Mac runs macOS {version}"),
            "update to macOS 27 or later, which includes the `fm` tool",
        )
    }
}

fn fm_binary(fm_path: &Path) -> Check {
    if fm_path.is_file() {
        Check::pass("`fm` tool", fm_path.display().to_string())
    } else {
        let fix = if std::env::var_os("FM_MCP_FM_PATH").is_some() {
            "FM_MCP_FM_PATH points at a missing file; unset it to use /usr/bin/fm".to_owned()
        } else {
            "`fm` ships with macOS 27; update macOS".to_owned()
        };
        Check::fail("`fm` tool", format!("{} not found", fm_path.display()), fix)
    }
}

/// Runs `fm <args>`; returns whether it succeeded and its output.
fn run_fm(fm_path: &Path, args: &[&str]) -> (bool, String) {
    match Command::new(fm_path)
        .args(args)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
    {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            let err = String::from_utf8_lossy(&o.stderr);
            if text.is_empty() {
                text = err.trim().to_owned();
            }
            (o.status.success(), text)
        }
        Err(e) => (false, e.to_string()),
    }
}

/// `fm license --status` prints "Agreed to license FM1 version 1.0 on …" once
/// agreed (2026-10-06). The not-agreed output is untested (Phase 6).
fn licence(fm_path: &Path) -> Check {
    let (ok, text) = run_fm(fm_path, &["license", "--status"]);
    if ok && text.starts_with("Agreed") {
        Check::pass("`fm` licence", text)
    } else {
        Check::fail(
            "`fm` licence",
            if text.is_empty() {
                "not agreed".into()
            } else {
                text
            },
            "run `fm license` in a terminal and agree to the terms",
        )
    }
}

/// `fm available` prints "System model available" and exits 0 when ready.
fn model_available(fm_path: &Path) -> Check {
    let (ok, text) = run_fm(fm_path, &["available"]);
    if ok {
        Check::pass("On-device model", text)
    } else {
        Check::fail(
            "On-device model",
            if text.is_empty() {
                "not available".into()
            } else {
                text
            },
            // No pane name: on macOS 27.0.1 the Siri pane has no Apple
            // Intelligence switch (checked 2026-10-09).
            "turn on Apple Intelligence for this Mac in System Settings, and wait for the \
             model to finish downloading",
        )
    }
}

fn socket_path() -> Check {
    let tmpdir = std::env::var_os("TMPDIR").map(std::path::PathBuf::from);
    match fm::private_socket_dir(tmpdir.as_deref()) {
        Ok((_dir, socket)) => Check::pass(
            "Socket path",
            format!(
                "{} bytes, under the 104-byte limit",
                socket.as_os_str().len()
            ),
        ),
        Err(e) => Check::fail(
            "Socket path",
            e.to_string(),
            "set TMPDIR to a short, writable folder, or make /tmp writable",
        ),
    }
}

async fn model_answers(config: FmConfig) -> Check {
    let backend = FmServe::new(config);
    let started = Instant::now();
    let request =
        ChatRequest::new(vec![ChatMessage::user("Reply with the word OK.")]).with_max_tokens(5);
    let result = backend.chat(request).await;
    let elapsed = started.elapsed().as_secs_f64();
    backend.shutdown().await;
    match result {
        Ok(response) if response.text().is_some_and(|t| !t.trim().is_empty()) => Check::pass(
            "Model test",
            format!("`fm serve` started and answered in {elapsed:.1} s"),
        ),
        Ok(_) => Check::fail(
            "Model test",
            "`fm serve` answered with no text",
            "run `fm-mcp doctor` again; if it repeats, report it as a bug",
        ),
        Err(e) => Check::fail(
            "Model test",
            e.to_string(),
            "fix the checks above first; if they pass, restart the Mac and try again",
        ),
    }
}

/// Checks 8–11: each agent's config, the guidance, and that the configured
/// binary is this one.
fn agent_checks(paths: &Paths) -> Vec<Check> {
    let configured = install::configured(paths);
    let this_binary = install::installed_binary().ok().map(|(path, _)| path);
    let mut checks = Vec::new();
    let mut commands = Vec::new();

    let agents = [
        (
            "Claude Code config",
            paths.claude_detected(),
            configured.claude,
            paths.claude_json.clone(),
            "--claude",
        ),
        (
            "Codex config",
            paths.codex_detected(),
            configured.codex,
            paths.codex_config(),
            "--codex",
        ),
    ];
    for (name, detected, command, file, flag) in agents {
        let fix = format!("run `fm-mcp install {flag}`");
        checks.push(match command {
            _ if !detected => Check::skip(name, "not installed"),
            Err(e) => Check::fail(name, format!("{e:#}"), fix),
            Ok(None) => Check::fail(name, format!("{} has no fm-mcp entry", file.display()), fix),
            Ok(Some(command)) if !Path::new(&command).is_file() => Check::fail(
                name,
                format!("fm-mcp is configured as {command}, which doesn't exist"),
                fix,
            ),
            Ok(Some(command)) => {
                commands.push((name, command.clone()));
                Check::pass(name, format!("runs {command}"))
            }
        });
    }

    checks.push(if !paths.claude_detected() {
        Check::skip("Claude Code skill", "Claude Code not installed")
    } else if paths.skill_file().is_file() {
        Check::pass(
            "Claude Code skill",
            paths.skill_file().display().to_string(),
        )
    } else {
        Check::warn(
            "Claude Code skill",
            "the fm-delegate skill is not installed, so Claude may not know when to delegate",
            "run `fm-mcp install --claude`",
        )
    });

    checks.push(if !paths.codex_detected() {
        Check::skip("Codex guidance", "Codex not installed")
    } else if install::snippet_installed(paths) {
        Check::pass(
            "Codex guidance",
            format!("in {}", install::agents_file(paths).display()),
        )
    } else {
        let read = install::agents_file(paths);
        let hidden = install::hidden_agents_file(paths);
        let hidden_has_block = std::fs::read_to_string(&hidden)
            .is_ok_and(|t| t.contains(crate::guidance::BEGIN_MARKER));
        let detail = if hidden_has_block {
            format!(
                "the fm-mcp rules are in {}, but Codex reads {} instead",
                hidden.display(),
                read.display()
            )
        } else {
            format!("{} has no fm-mcp rules", read.display())
        };
        Check::fail("Codex guidance", detail, "run `fm-mcp install --codex`")
    });

    let canonical = |p: &str| Path::new(p).canonicalize().ok();
    let this = this_binary.as_ref().and_then(|p| p.canonicalize().ok());
    let stale: Vec<String> = commands
        .iter()
        .filter(|(_, command)| canonical(command) != this)
        .map(|(name, command)| format!("{name} runs {command}"))
        .collect();
    checks.push(
        match (&this_binary, stale.is_empty(), commands.is_empty()) {
            (_, _, true) => Check::skip("Configured binary", "no agent is configured yet"),
            (Some(path), true, false) => {
                Check::pass("Configured binary", format!("matches {}", path.display()))
            }
            (path, _, false) => Check::warn(
                "Configured binary",
                format!(
                    "{}, not this fm-mcp ({})",
                    stale.join("; "),
                    path.as_ref()
                        .map_or("unknown".into(), |p| p.display().to_string())
                ),
                "run `fm-mcp install` with the fm-mcp you want agents to use",
            ),
        },
    );
    checks
}
