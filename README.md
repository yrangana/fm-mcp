# fm-mcp

An MCP server that lets coding agents such as Claude Code and Codex hand simple text work to the on-device model for Apple Foundation Models, the `fm` that ships with macOS 27. It's free, private, and runs offline.

> **Status: in development, not released yet.** It works from source today with four tools, `summarise`, `extract`, `classify` and `ocr`, plus `fm-mcp install` and `fm-mcp doctor`. Homebrew installation is being built. See [Roadmap](#roadmap).

## Why

Coding agents spend paid, remote tokens on simple jobs, like summarising a log or pulling fields out of a document. A small model already on your Mac can do much of that for nothing, and your text never leaves the machine. fm-mcp lets the agent hand off that work, and its tool descriptions tell the agent plainly what the small model can't do.

## Good for, and not for

- **Good for:** summarising logs, documents, notes and transcripts; pulling fields out of text as JSON; sorting text into your categories; reading text in screenshots and photos.
- **Not for:** code, maths, reasoning, facts the text doesn't contain, or anything where a wrong answer is costly and you can't check it.
- **Limits:** the model has a small context of about 8K tokens, shared by your input and its answer. Summaries can miss or distort details, so check anything important.

## Requirements

- A Mac with Apple Silicon.
- macOS 27 with Apple Intelligence turned on.
- The `fm` licence accepted once: run `fm license`.

## Build and try it

```sh
git clone https://github.com/yrangana/fm-mcp.git
cd fm-mcp
cargo build --release
./target/release/fm-mcp install --dry-run   # shows what would change
./target/release/fm-mcp install             # configures Claude Code and Codex
./target/release/fm-mcp doctor              # checks everything fm-mcp needs
```

`install` configures every agent it finds; `--claude` or `--codex` picks one. It backs up each file before changing it, prints every path it touched, and running it again changes nothing. It also installs a Claude Code skill and an AGENTS.md section for Codex that say when to delegate; `--no-guidance` skips them. `fm-mcp install --uninstall` removes everything it added.

Then start a new session and ask Claude Code, for example, to "summarise this log with fm-mcp" or "read the text in this screenshot with fm-mcp". fm-mcp starts `fm serve` on its own the first time a tool is called, and stops it when the session ends.

## Development

```sh
cargo test --features fake-fm   # needs no Apple Intelligence: uses a fake fm
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```

[AGENTS.md](AGENTS.md) has the architecture, the tested facts about `fm serve`, and the project rules.

## Roadmap

- [x] `summarise` over `fm serve`, with restarts, clear errors and clean shutdown
- [x] `extract` (JSON Schema), `classify`, `ocr`; long input for `summarise`
- [x] Guidance that tells agents when to delegate (a Claude Code skill and an AGENTS.md snippet)
- [x] `fm-mcp install` (sets up Claude Code and Codex) and `fm-mcp doctor`. The Codex setup still needs a check on a real Codex install.
- [ ] Release: Homebrew (`brew install yrangana/tap/fm-mcp`), a shell installer, the MCP Registry and a Claude plugin

## Licence

[MIT](LICENSE). fm-mcp is not affiliated with or endorsed by Apple. It calls the `fm` tool installed on your Mac and includes nothing from Apple.
