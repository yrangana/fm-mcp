# Changelog

One entry per release. The release workflow uses the entry as the GitHub release notes.

## Unreleased

- README: installing Homebrew first asks to install Xcode's command line tools.
- README: `extract` limits are more cautious. New tests showed a missing field can come back as a wrong value or the "got stuck" error, not only `null`, and that `$1,280.00` in a `number` field needs a line in `instructions`.
- New `docs/TOOLS.md`: every tool and parameter, generated from the descriptions the server sends, with example calls.
- `CHANGELOG.md`, `SECURITY.md` and issue templates.

## 0.1.0 - 2026-10-11

First release, a **public beta**. It needs an Apple Silicon Mac with macOS 27 and Apple Intelligence on; see [Install](https://github.com/yrangana/fm-mcp#install) for what's supported.

- Four tools on the on-device model for Apple Foundation Models: `summarise`, `extract`, `classify` and `ocr`. `summarise` splits and combines long input.
- `fm-mcp install` sets up Claude Code and Codex, with delegation guidance for each; `fm-mcp doctor` checks everything and says how to fix it.
- Install with Homebrew (`brew install yrangana/tap/fm-mcp`), the shell installer, `cargo install fm-mcp`, or the Claude Code plugin. Listed in the MCP Registry as `io.github.yrangana/fm-mcp`.
- Tested with Claude Code, Codex, Claude Desktop and Devin.
