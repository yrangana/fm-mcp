# Manual test before a release

Run this on a real Mac (Apple Silicon, macOS 27, Apple Intelligence on) before every release. CI only tests against a fake `fm`. Copy the checklist into the release PR and tick it there. If an item fails, open an issue and link it.

Write down the macOS build (`sw_vers`) and the Claude Code and Codex versions (`claude --version`, `codex --version`).

## 1. Smoke script

- [ ] `scripts/real_fm_smoke.sh` passes: it checks the facts about `fm` that AGENTS.md relies on and calls each tool through the binary.
- [ ] `scripts/real_fm_smoke.sh sessions` passes: three sessions at once, and an `extract` running alongside a long `summarise`. Warnings are fine; record their numbers in the release PR.

A failed fact check means `fm` changed. Update AGENTS.md "Verified facts" with the date, and teach the fake (`tests/fake_fm/fake_fm.rs`) the new behaviour.

## 2. Fresh install

Use the release you are about to ship, not a local build.

- [ ] Remove any earlier setup: `fm-mcp install --uninstall`, `brew uninstall fm-mcp`.
- [ ] `brew install yrangana/tap/fm-mcp` installs with no warnings.
- [ ] `fm-mcp install --dry-run` lists the changes and writes nothing (`git diff --no-index` or a timestamp check on `~/.claude.json`).
- [ ] `fm-mcp install` reports every path it touched. Running it a second time reports nothing changed.
- [ ] `fm-mcp doctor` shows every check passing.

## 3. Claude Code

In a new session:

- [ ] `/mcp` lists fm-mcp, connected, with 4 tools.
- [ ] "Summarise `<a log of a few thousand lines>` with fm-mcp." The result arrives, with progress updates if the input is long. Check what it covers as well as whether it's right.
- [ ] "Pull the invoice number, total and due date out of `<file>` with fm-mcp." The fields are right; a field the text lacks is `null`.
- [ ] "Sort these 10 support messages into billing, bug, feature request with fm-mcp."
- [ ] "Read the text in `<screenshot.png>`." The text is right.
- [ ] Without naming fm-mcp: "What errors are in this log?" on a long log. Note whether the skill made it delegate.
- [ ] Without naming fm-mcp: "Refactor this function." It must **not** delegate.

## 4. Codex

The same five tool prompts as in section 3, in a new Codex session.

- [ ] `codex mcp list` (or `/mcp` in the TUI) lists fm-mcp.
- [ ] Each of the four tools works.
- [ ] Ask Codex to quote its fm-mcp guidance; it quotes the `AGENTS.md` block.
- [ ] Override file: create `~/.codex/AGENTS.override.md` empty, then with only whitespace, then with one line of text. Each time, run `fm-mcp doctor` and ask Codex to quote its fm-mcp guidance. `doctor` must agree with what Codex loads.

## 5. Failures

- [ ] Kill `fm serve` mid-session (`pkill -9 -f 'fm serve --socket'`, which hits every session's `fm serve`). The next tool call works.
- [ ] Quit the agent. `pgrep -fl 'fm serve --socket'` shows no `fm-mcp-*` socket from that session, and `ls $TMPDIR | grep fm-mcp-` is empty once all sessions are closed.
- [ ] Turn Apple Intelligence off in System Settings. Record exactly what `fm available` prints, what `doctor` shows, and the tool error an agent gets. Turn it back on.
- [ ] Licence not agreed, if it can be reset: record what `fm license --status`, `doctor` and a tool call show.

## 6. Uninstall

- [ ] `fm-mcp install --uninstall` reports what it removed.
- [ ] `~/.claude.json`, `~/.codex/config.toml` and `~/.codex/AGENTS.md` are as they were before the install, apart from fm-mcp's backups.
- [ ] `~/.claude/skills/fm-delegate` is gone.
