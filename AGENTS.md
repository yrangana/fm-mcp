# AGENTS.md

Guidance for coding agents (Claude Code, Codex) working in this repository.

## What this repo is

**fm-mcp** — an MCP server that lets coding agents hand cheap, private, simple work to Apple's on-device Foundation Model (the `fm` that ships with macOS 27). Free, offline, one-command install.

**Status:** v1 is being built in phases. Current state: `plans/STATUS.md` and `plans/active/FM_MCP_V1.md`.

## The goal

A developer runs `brew install yrangana/tap/fm-mcp && fm-mcp install`, and Claude Code and Codex can immediately delegate summarising, extraction, classification and OCR to the local model — without Python, cloning, or hand-editing config.

Other Apple FM MCP servers exist, all Python and clone-to-install. What sets this one apart:

1. **One Rust binary via Homebrew** — no runtime, no clone, no venv.
2. **`fm-mcp install`** writes Claude Code and Codex config; **`fm-mcp doctor`** explains anything missing.
3. **Built for delegation** — ships a Claude Code skill and an `AGENTS.md` snippet telling the agent *when* to offload. Tool descriptions state limits honestly.
4. **Handles the model's limits** — chunks long input; turns context and guardrail errors into clear, actionable messages.

**Not:** a coding model, a replacement for Claude, or Windows/Linux support in v1.

## v1 scope

- **Tools:** `summarise`, `extract` (JSON schema), `classify`, `ocr`. No general `ask` tool in v1 (decided 2026-10-06: it invites over-delegation).
- **Commands:** `fm-mcp` (stdio server), `fm-mcp install [--claude] [--codex] [--dry-run] [--uninstall] [--no-guidance]`, `fm-mcp doctor`.
- **Requirements:** Apple Silicon, macOS 27, Apple Intelligence enabled.

## Architecture

- **Rust**, MCP via the official `rmcp` SDK, **stdio** transport.
- **Model access through `fm serve`**, which fm-mcp spawns as a child process on a Unix socket, supervises, and shuts down on exit.
- **Orphan protection.** Each `fm serve` gets a watchdog, a hidden `fm-mcp __watch` mode of the same binary. If fm-mcp is force-killed and cannot clean up, the watchdog stops `fm serve` and deletes its socket folder. At startup, fm-mcp also stops any of the user's orphaned `fm serve --socket …/fm-mcp-*/fm.sock` processes (parent pid 1) and deletes stale `fm-mcp-*` folders. Code: `src/orphans.rs`.
- **`fm` CLI only for what `serve` cannot do:** token counting (`fm count-tokens -q`). OCR goes through `fm serve` vision, which tested better than `fm respond --tool ocr` (see verified facts).
- **No Swift bridge, no SDK.** There is no Rust SDK for FoundationModels; bridging to Swift is not worth the build cost.
- **Backend behind a trait**, so Ollama or Foundry Local can be added later (both speak the same Chat Completions format). v1 ships the `fm` backend only.

## Verified facts about `fm serve`

Probed on macOS 27, 2026-10-06; re-checked on macOS 27.0.1 (26A434), 2026-10-06. Re-check if behaviour seems off.

- **Streams by default** even without `stream: true`. Always send `"stream": false`.
- **Structured output works** via `response_format: {type: "json_schema", ...}`.
- **Images work** via `image_url` content parts with base64 data URLs. Plain vision read printed text from a PNG correctly.
- **`tools` (function calling) is ignored, not rejected** *(changed 2026-10-06)*. A well-formed `tools` array returns HTTP 200 with no `tool_calls`. A function with no `description` returns HTTP 400 *"Invalid JSON: The data couldn't be read because it is missing."* The request decoder is strict about missing fields. Not needed — never send `tools`.
- **`system` messages, `temperature` and `max_tokens` are accepted** *(2026-10-06)*, **but `max_tokens` is ignored; only `max_completion_tokens` caps the answer** *(2026-10-09)*.
  - Asked for a 600-word essay with a cap of 30: `max_tokens` gave 587–699 answer tokens, `max_completion_tokens` exactly 30, in 1.8 s.
  - A capped answer still has `finish_reason: "stop"`, so the only sign of a cut is `completion_tokens` equal to the cap. In structured output `fm` closes the JSON, so a cut answer parses but can end in junk.
  - Found through Codex: `summarise` overflowed the context on a 22K-token changelog because a part summary capped at 400 tokens ran on past 1,500.
- **Context is about 8K tokens** *(measured 2026-10-06)*. Prompts up to 7,883 tokens succeeded; about 8.2K failed. The real budget is 8K minus the output length.
- **Context overflow** → HTTP 500, message *"The session's transcript exceeded the model's context size."*
- **Guardrails** → HTTP 500, message *"The model's safety guardrails were triggered."* Fires on benign input too.
  - *(2026-10-06)* Repeated text did not trigger it within the context limit: one sentence repeated 300 times, about 5.4K tokens, passed.
  - An input far over the limit (about 14K tokens of repeated text) returned the guardrail error *instead of* the overflow error. Do not rely on the error type to detect oversized input; count tokens first.
- **No log probabilities** *(probed 2026-10-07)*. `logprobs: true` with `top_logprobs: 5` returns HTTP 200 but no `logprobs` field, for plain and structured (`enum`) answers alike. A `logprobs` value of the wrong type is also accepted, so the field is ignored. Streaming chunks carry text only, and `fm respond --verbose` shows no probabilities. There is no way to get per-option probabilities or a confidence score from `fm`.
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
- **Measured through fm-mcp on 2026-10-07 (macOS 27.0.1):**
  - `summarise` on a 30,288-token log took 86.9 s: 5 parts, then combined, with 6 progress updates. A second run with different prompts took 130 s. The summary kept 2 of 3 planted incidents exactly; the third was lost when summarising its part (see plan R12).
  - `extract`: 1.2 s for 3 fields and 2.0 s with an array of line items. A field missing from the text came back as `null` with the nullable rewrite, with no runaway.
  - `classify`: 0.5 s per call. Over 20 varied support messages with 4 labels, 20 of 20 answers were valid labels and 18 of 20 were the expected label.
  - `ocr`: about 2.1 s per image. 30 of 30 known phrases across the 5 test images.
- **Token density varies a lot by text type** *(measured 2026-10-07)*. Characters per token: prose (AGENTS.md) 3.5, Rust code 3.5, random dictionary words 4.2, timestamped logs with IDs and numbers **1.6**. A fixed characters-per-token estimate undercounts logs by more than half, so always count real tokens. `fm count-tokens -q` over stdin takes about 0.08 s (1.3 s on the first, cold call). Empty input fails with *"Missing prompt."*, so never send empty text.
  - **Words per token in prose, for the limits the tools quote** *(measured 2026-10-07 on repeated README text)*: 3,640 words = 5,840 tokens; 4,095 words = 6,570; 5,005 words = 8,030. So about 1.6 tokens per word. The `extract` input budget is 6,518 tokens and `classify` 7,318, which is why both tools say "about 3,500 words"; 5,000 words is refused as too long. The `summarise` cap of 48,000 tokens is about 30,000 words.
- **Structured output: which JSON Schema features work** *(probed 2026-10-07 on a short invoice)*.
  - **Work:** flat objects (string, number, integer, boolean), nested objects, arrays of strings, arrays of objects, `enum`, `const`, `minItems`/`maxItems`, `additionalProperties: false`, and a top-level array (messy output).
  - **`anyOf`:** needs a `title` on the property that holds it, e.g. `{"title": "Total", "anyOf": [...]}`; without one, HTTP 400 *"AnyOf schemas require a 'title' key"*. Titles on the branches don't help.
  - **Rejected with HTTP 400:** `$ref` (both `#/$defs/…` and `#/definitions/…`: *"undefinedReferences"*), and nullable type arrays such as `["string", "null"]`.
  - **`pattern`:** HTTP 500 *"An unsupported generation guide was used."*
  - **`format: "date"`:** accepted but ignored ("2 April 2026" came back).
  - **`minimum`/`maximum`:** enforced by bending the answer. With `maximum: 5` the total came back as `4.5` (true value 71.5). Constraints don't validate; they force a wrong value.
- **Structured output can run away when the text lacks a requested field** *(2026-10-07)*. The model fills the string with junk until the context overflows. In 3 of 4 runs that took about 200 s and ended in the overflow error; the 4th returned junk (`"not available', 1, 2026-03-03, "`). This happened with optional and required fields, and with "or empty string if none" in the field description. An array of objects ran away once and worked in 2 s another time. A tool that uses structured output must cap the answer and must not let a missing field look like a real value.
  - **`max_tokens` does not stop a runaway** *(2026-10-07)*. With `max_tokens: 300`, both runs were still generating at 150 s. **`max_completion_tokens` does** *(2026-10-09)*: with a cap of 200, a runaway stopped at 200 tokens after 3.5 s, as valid JSON ending in junk. fm-mcp sends `max_completion_tokens` and treats an answer that uses the whole cap as a runaway.
  - **`fm serve` keeps generating after the client disconnects**, so later requests queue behind a runaway. The only cure is restarting `fm serve`.
  - **A "use NOT_FOUND if missing" instruction doesn't help.** It copied the wrong field's value, or junk.
  - **What works: `anyOf: [<type>, {"type": "null"}]` with a `title`.** Missing fields come back as `null`. On a flat schema with 4 present and 2 missing fields, 5 of 5 runs were exactly right, in about 1.4 s each. A `{found, value}` pair also worked, 2 of 2.
  - **Titles must be unique and specific, e.g. `ItemSku`.** With short titles (`name`, `qty`) on two or more `anyOf` fields inside array items, 6 of 6 runs ran away. With path-style titles (`ItemName`, `ItemQty`, `ItemSku`), 4 of 4 were right.
  - **A missing field close in meaning to other text can take that text instead of `null`** *(2026-10-08, flat schemas, `anyOf` + title)*. On an invoice with no PO number, `purchase_order` got the line items, and `tax_id` the supplier's name. Missing fields unrelated to the text (tracking number, phone, office address) were `null` in 20 of 20 runs. Two changes help: the system prompt adds "Most of the text belongs to no field: leave it out. A field gets a value only when the text states that exact thing", and each field's description ends "Null unless the text states it.". Interleaved on one `fm serve`, 30 runs each: `purchase_order` was `null` 1 of 30 times before and 20 of 30 after; the present fields were right 25 of 30 before and 30 of 30 after. `extract` sends both, but about 1 in 3 such fields is still wrong. Other shapes did not help: the `null` branch first in `anyOf` (no change), a `{stated, value}` pair per field (present fields 15 of 18), a leading array of missing field names (ran away). Run-to-run variance is large, so compare variants interleaved and with 20+ runs.
  - **Nested objects remain unreliable.** Even with unique titles, 1 of 2 runs ran away. In 2 runs the model put the email address into a missing `phone` field: a plausible but wrong value instead of `null`.
- **OCR: plain vision through `fm serve` beats `fm respond --tool ocr`** *(compared 2026-10-07 on 5 images: a screenshot, small text, two columns, a receipt, handwriting-style text)*.
  - **Vision via `fm serve`** (`image_url` data URL, prompt "Return all the text in this image exactly as written, line by line"): 30/30 known phrases, about 1.8 s per image, layout kept (receipt lines like `Flat white 4.80`), no preamble.
  - **`fm respond --no-stream --tool ocr --image <file>`:** 28/30 known phrases, about 3.3 s per image. It garbled small text (`201 B`, `help¿@`), separated receipt prices from their items, and sometimes added "Here is the text…".
  - **Image types accepted by `fm serve` vision:** PNG, JPEG, HEIC, TIFF, GIF and BMP, each with its own MIME type in the data URL; each read the receipt total correctly. WebP is untested.
  - **PDF is not supported:** passing a PDF to `fm respond --image` fails with *"The prompt contains content that the model cannot process."*
- **Licence gate:** `fm` has a Legal Notice that must be agreed once (`fm license`). `fm license --status` reports it. `fm available` prints *"System model available"* and exits 0 when the model is ready *(2026-10-06)*.
- `fm` has no `--version` flag.
- Endpoints: `GET /health`, `GET /v1/models` (model id `system`), `POST /v1/chat/completions`.

## Verified facts about the agents' config

Checked 2026-10-08 with Claude Code 2.1.293, in a scratch `HOME`.

- **`claude mcp add -s user <name> -- <command>`** writes `~/.claude.json`, or `$CLAUDE_CONFIG_DIR/.claude.json` when that is set, as `mcpServers.<name> = {"type": "stdio", "command", "args", "env": {}}`. It honours `HOME`. It also saves its own copy under `~/.claude/backups/`.
- **`add` exits 1** with *"MCP server … already exists in user config"* if the name is taken, so `install` removes a stale entry first. **`claude mcp remove <name> -s user`** exits 1 with *"No MCP server named …"* if it is missing.
- **`~/.claude.json` is written with 2-space indents and no final newline.** `install` keeps that layout and the key order when it edits the file directly (no `claude` on `PATH`).
- **Codex verified on a real install** *(2026-10-09, Codex CLI 0.162.0)*. `codex mcp list`/`get` show the `[mcp_servers.fm-mcp]` entry `install` writes (`command`, empty `args`, `tool_timeout_sec`), and Codex sessions call the tools.
  - **The global AGENTS rule holds:** Codex reads `~/.codex/AGENTS.override.md` instead of `AGENTS.md` only when the override has non-whitespace text. With no override, an empty one or a whitespace-only one, Codex quoted the fm-mcp rules from `AGENTS.md`; with one line of text it saw only the override, and `doctor` reported the same each time.

## Development

- **Run tests with `cargo test --features fake-fm`.** The feature builds `fake-fm` (`tests/fake_fm/fake_fm.rs`), a fake `fm serve` the integration tests use, so they need no Apple Intelligence. Plain `cargo test` fails on purpose with that instruction.
- **Lint:** `cargo clippy --all-targets --all-features -- -D warnings` and `cargo fmt --check`. CI (`.github/workflows/ci.yml`) runs all three on macOS arm64.
- **The fake is driven by** environment variables (`FAKE_FM_AVAILABLE`, `FAKE_FM_LICENSE`, `FAKE_FM_START_DELAY_MS`, `FAKE_FM_LOG`) and magic words in the prompt (`FAKE_OVERFLOW`, `FAKE_GUARDRAIL`, `FAKE_BAD_REQUEST`, `FAKE_HANG`, `FAKE_CRASH`, `FAKE_CRASH_ONCE`, `FAKE_REPEAT`, `FAKE_RUN_ON`). When `fm` shows new behaviour, record it in the verified facts above and teach the fake.
- **Settings:** `FM_MCP_FM_PATH` (default `/usr/bin/fm`), `FM_MCP_REQUEST_TIMEOUT_SECS` (default 120), `FM_MCP_LOG` (log filter, default `info,rmcp=warn`). Logs go to stderr only; stdout is the MCP protocol.
- **`install` and `doctor`** (`src/install/`, `src/doctor.rs`) are tested in `tests/install.rs` against a scratch `HOME` with `PATH` limited to `/usr/bin:/bin`, so the real `claude` and your real config are never touched. A fake `claude` script stands in when a test needs one. To try them by hand, set `HOME` to a scratch folder too, or use `--dry-run`.
- **Delegation guidance** lives in `skills/fm-delegate/SKILL.md` (Claude Code skill) and `snippets/AGENTS.md.snippet` (Codex). Both are embedded in the binary by `src/guidance.rs`. Change them only after testing decisions with sub-agents before and after (see the plan's Phase 4 notes), and keep their size limits in step with the `summarise` description.
- **Tool schemas must be portable:** one `type` per field and no `$ref`. A unit test enforces this for every tool.

## Distribution

- **Homebrew formula** in our own tap — not a cask. Formulae are not quarantined, so **no Apple Developer account or notarisation** is needed.
- Also: `curl | sh` installer and `cargo install`.
- Releases built with **`cargo-dist`** on a GitHub Actions macOS arm64 runner.
- Publish to the **MCP Registry** (`server.json`, `mcp-publisher`) and the **Claude plugin marketplace** (plugin = MCP config + delegation skill).
- CI cannot run Apple Intelligence — test against a fake `fm serve`; do a manual pass on a real Mac before each release.

## Open decisions

- **GitHub owner** — decided 2026-10-07: `yrangana`. Repo `github.com/yrangana/fm-mcp`, tap `yrangana/homebrew-tap`, MCP Registry name `io.github.yrangana/fm-mcp`.
- **Licence** — decided 2026-10-07: **MIT** (`LICENSE`).

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
