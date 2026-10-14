# Changelog

One entry per release. The release workflow uses the entry as the GitHub release notes.

## 0.1.2 - 2026-10-12

- `classify` now names the labels in the prompt. Before, the model saw them only in the answer's schema and often didn't know its choices: on 200 news articles (AG News, 4 topics) it got 49% right, now 64%; on 204 sentences labelled with 6 emotions, 29% then, 58% now.
- When the model answers "The model refused to answer.", fm-mcp now says so and tells the agent to do the task itself, instead of calling it a bug in fm-mcp.
- New project site at https://yrangana.github.io/fm-mcp/, with notes on what was learned building fm-mcp.

## 0.1.1 - 2026-10-11

- `extract` reads amounts in `number` and `integer` fields itself: the model copies `$1,280.00` or `AUD 12,450.50` as text and fm-mcp turns it into a number, or `null` if it isn't plainly one number. Before, such amounts often came back `null`. Over 5 invoices, `total` was right in every answer the model finished; 0.1.0 got 40 of 50 right.
- `extract` keeps the finished fields when the model gets stuck on one, on a flat schema: the stuck field is set to `null`, and a note names it. Before, the whole answer was discarded with a "got stuck" error. With two fields the text lacked, 0 of 25 calls returned an error (0.1.0: 13 of 25), though a missing field close in meaning can still take a wrong value.
- README: installing Homebrew first asks to install Xcode's command line tools.
- README and the `extract` description: a field the text lacks can come back as a wrong value from nearby text, not only `null`.
- New `docs/TOOLS.md`: every tool and parameter, generated from the descriptions the server sends, with example calls.
- `CHANGELOG.md`, `SECURITY.md` and issue templates.

## 0.1.0 - 2026-10-11

First release, a **public beta**. It needs an Apple Silicon Mac with macOS 27 and Apple Intelligence on; see [Install](https://github.com/yrangana/fm-mcp#install) for what's supported.

- Four tools on the on-device model for Apple Foundation Models: `summarise`, `extract`, `classify` and `ocr`. `summarise` splits and combines long input.
- `fm-mcp install` sets up Claude Code and Codex, with delegation guidance for each; `fm-mcp doctor` checks everything and says how to fix it.
- Install with Homebrew (`brew install yrangana/tap/fm-mcp`), the shell installer, `cargo install fm-mcp`, or the Claude Code plugin. Listed in the MCP Registry as `io.github.yrangana/fm-mcp`.
- Tested with Claude Code, Codex, Claude Desktop and Devin.
