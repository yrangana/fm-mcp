# AGENTS.md

Guidance for coding agents (Claude Code, Codex) working in this repository.

## What this repo is

**fm-mcp** — an MCP server that lets coding agents hand cheap, private, simple work to Apple's on-device Foundation Model (the `fm` that ships with macOS 27). Free, offline, one-command install.

**There is no code yet.** Planning comes first, under `plans/`.

## The goal

A developer runs `brew install <owner>/tap/fm-mcp && fm-mcp install`, and Claude Code and Codex can immediately delegate summarising, extraction, classification and OCR to the local model — without Python, cloning, or hand-editing config.

Other Apple FM MCP servers exist, all Python and clone-to-install. What sets this one apart:

1. **One Rust binary via Homebrew** — no runtime, no clone, no venv.
2. **`fm-mcp install`** writes Claude Code and Codex config; **`fm-mcp doctor`** explains anything missing.
3. **Built for delegation** — ships a Claude Code skill and an `AGENTS.md` snippet telling the agent *when* to offload. Tool descriptions state limits honestly.
4. **Handles the model's limits** — chunks long input; turns context and guardrail errors into clear, actionable messages.

**Not:** a coding model, a replacement for Claude, or Windows/Linux support in v1.

## v1 scope

- **Tools:** `summarise`, `extract` (JSON schema), `classify`, `ocr`, possibly a general `ask`.
- **Commands:** `fm-mcp` (stdio server), `fm-mcp install [--claude] [--codex]`, `fm-mcp doctor`.
- **Requirements:** Apple Silicon, macOS 27, Apple Intelligence enabled.

## Architecture

- **Rust**, MCP via the official `rmcp` SDK, **stdio** transport.
- **Model access through `fm serve`**, which fm-mcp spawns as a child process on a Unix socket, supervises, and shuts down on exit.
- **`fm` CLI only for what `serve` cannot do:** OCR and barcode (`fm respond --tool ocr|barcode`) and token counting (`fm count-tokens -q`).
- **No Swift bridge, no SDK.** There is no Rust SDK for FoundationModels; bridging to Swift is not worth the build cost.
- **Backend behind a trait**, so Ollama or Foundry Local can be added later (both speak the same Chat Completions format). v1 ships the `fm` backend only.

## Verified facts about `fm serve`

Probed on macOS 27, 2026-10-06; re-checked on macOS 27.0.1 (26A434), 2026-10-06. Re-check if behaviour seems off.

- **Streams by default** even without `stream: true`. Always send `"stream": false`.
- **Structured output works** via `response_format: {type: "json_schema", ...}`.
- **Images work** via `image_url` content parts with base64 data URLs. Plain vision read printed text from a PNG correctly.
- **`tools` (function calling) is ignored, not rejected** *(changed 2026-10-06)*. A well-formed `tools` array returns HTTP 200 with no `tool_calls`. A function with no `description` returns HTTP 400 *"Invalid JSON: The data couldn't be read because it is missing."* The request decoder is strict about missing fields. Not needed — never send `tools`.
- **`system` messages, `temperature` and `max_tokens` are accepted** *(2026-10-06)*.
- **Context is about 8K tokens** *(measured 2026-10-06)*. Prompts up to 7,883 tokens succeeded; about 8.2K failed. The real budget is 8K minus the output length.
- **Context overflow** → HTTP 500, message *"The session's transcript exceeded the model's context size."*
- **Guardrails** → HTTP 500, message *"The model's safety guardrails were triggered."* Fires on benign input too.
  - *(2026-10-06)* Repeated text did not trigger it within the context limit: one sentence repeated 300 times, about 5.4K tokens, passed.
  - An input far over the limit (about 14K tokens of repeated text) returned the guardrail error *instead of* the overflow error. Do not rely on the error type to detect oversized input; count tokens first.
- **Unknown request fields are accepted silently** (e.g. `"guardrails": "..."`), so there is no evidence `serve` exposes a guardrail setting. `fm respond` does have `--guardrails permissive-content-transformations` *(2026-10-06)*.
- **Requests are processed one at a time** *(2026-10-06)*. Three parallel requests finished at 5.6s, 8.2s and 11.0s. One summary of 3.7K tokens takes about 3.7s.
- **Several `fm serve` processes also queue; they don't run in parallel or fail** *(2026-10-06)*. Each process had its own socket, which is what separate fm-mcp sessions would do.
  - Three processes, one simultaneous 3.7K-token request each, over three runs: finished at about 5–6s, 8–9s and 10.5–11.5s. All returned HTTP 200.
  - Three processes with two requests each (6 total) finished between 7.5s and 20.9s, all 200.
  - The model is shared system-wide: each extra request adds about 2.6s, whichever process sends it. Running more `serve` processes adds no throughput, and nothing failed.
- **`fm serve` removes its socket file when it receives SIGTERM** *(2026-10-06)*.
- **Unix socket paths must be short** (macOS limit 104 bytes). A long path fails silently: `fm serve` keeps running and prints nothing, but no socket file is created. Re-confirmed 2026-10-06 with a 169-byte path. `$TMPDIR/fm-mcp.sock` is about 60 bytes here. Check the length.
- **`fm serve` prints nothing on startup** (stdout and stderr redirected). Poll `GET /health` for readiness; it returns `{"models":[{"available":true,"name":"system"}],"status":"fm serve is running"}` *(2026-10-06)*.
- Non-streaming responses include `usage` token counts. `prompt_tokens` is about 55–60 more than `fm count-tokens -q` on the text alone, which is the chat framing overhead.
- **OCR:** `fm respond --no-stream --tool ocr --image <file> '<prompt>'` works *(2026-10-06)*.
- **Licence gate:** `fm` has a Legal Notice that must be agreed once (`fm license`). `fm license --status` reports it. `fm available` prints *"System model available"* and exits 0 when the model is ready *(2026-10-06)*.
- `fm` has no `--version` flag.
- Endpoints: `GET /health`, `GET /v1/models` (model id `system`), `POST /v1/chat/completions`.

## Distribution

- **Homebrew formula** in our own tap — not a cask. Formulae are not quarantined, so **no Apple Developer account or notarisation** is needed.
- Also: `curl | sh` installer and `cargo install`.
- Releases built with **`cargo-dist`** on a GitHub Actions macOS arm64 runner.
- Publish to the **MCP Registry** (`server.json`, `mcp-publisher`) and the **Claude plugin marketplace** (plugin = MCP config + delegation skill).
- CI cannot run Apple Intelligence — test against a fake `fm serve`; do a manual pass on a real Mac before each release.

## Open decisions

- **GitHub owner** for the repo and tap — undecided. Use `<owner>` as a placeholder; do not invent one.
- **Licence** — proposed `MIT OR Apache-2.0`, not confirmed.

## Hard rules

- **No "Apple" or "Apple Intelligence" in the product, crate, tap or binary name** — trademark risk. "For Apple Foundation Models" in descriptions is fine.
- **Never claim a capability you have not tested against real `fm`.** Record new findings in the verified-facts list above, with the date.
- **Tool descriptions must state the limits** (small context, not for code or reasoning). An agent that over-delegates is a worse failure than one that under-delegates.

## Writing style

Plain English, short, lead with the answer. No invented jargon.

## Project Status & Plan Management

This project uses the `plans/` convention. Two files anchor everything:

- `plans/STATUS.md` is the front door. It shows current state (in flight, up next, recently shipped). Read it at the start of every session.
- `plans/README.md` is the convention itself. It covers plan file format, frontmatter spec, the two-source rule, lifecycle, directory layout, and quick rules. Read it whenever you engage with the `plans/` system in a session, and re-consult when uncertain. It is the source of truth for the convention.

Non-deferrable operational rules (do not violate even if `plans/README.md` is not yet loaded):

- **Write plans only in `plans/active/`.** When the user asks you to plan, design, or spec out non-trivial work, scaffold the file with `/plans new`. Never save plans to an assistant scratch or memory directory (`~/.claude/`, `~/.cursor/`, `.agents/`, `.windsurf/`, or equivalent), to plan-mode artifacts, or to loose files at the repo root. Plans written outside `plans/active/` are invisible to collaborators, the dashboard, and `/plans sync`.
- **`plans/plans.json` is auto-generated.** Never hand-edit it.
- **Update plans before ending a session.** If the session touched code covered by an active plan, update that plan's `## Status` banner and bump `last_updated` in its frontmatter before finishing. Do not defer to a follow-up session.
- **Audit drift with `/plans sync`.** Run it periodically, and after notable git activity, to reconcile plans against git log and regenerate `plans.json` and `STATUS.md` auto-sections.

Anything not covered above (frontmatter fields, status banner format, two-source rule, idea lifecycle, moves between `active/`, `shipped/`, and `superseded/`, the dashboard, file-move conventions, etc.) lives in `plans/README.md`. Defer to that file rather than guessing.
