# Security

## Reporting a problem

Please report security problems privately, through [GitHub's private vulnerability reporting](https://github.com/yrangana/fm-mcp/security/advisories/new), not in a public issue. Expect a reply within a week.

Supported versions: the latest release only.

## What fm-mcp does

- **No network.** fm-mcp sends nothing over the network. Text and images go only to the on-device model, through `fm serve` on the same Mac.
- **A private socket.** Each session starts its own `fm serve` on a Unix socket in a new folder with a random name and mode 0700, so other users on the Mac can't reach it. The folder is deleted when the session ends; a watchdog cleans it up if fm-mcp is killed.
- **Reads only what it's given.** The tools read a file only when the agent passes its `path`: text files up to 1 MB, or one image. They write no files.
- **`fm-mcp install`** edits only the agents' own config: `~/.claude.json`, `~/.claude/skills/fm-delegate/`, `~/.codex/config.toml` and `~/.codex/AGENTS.md` (or `AGENTS.override.md`). It backs up each file before changing it, keeps a record of its changes in `~/Library/Application Support/fm-mcp/install.json`, and `--uninstall` removes what it added.
