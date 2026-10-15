# Demo recordings

Two short GIFs for the README and the site, recorded with [vhs](https://github.com/charmbracelet/vhs) from the `.tape` files here, so a retake comes out the same:

- `setup.gif` (about 20 s): `brew install`, `fm-mcp install`, `fm-mcp doctor` with every check passing.
- `delegate.gif` (about 35 s): Claude Code asked what went wrong in a log; it calls fm-mcp's `summarise`, and answers from the summary.

Two clips instead of one: together they run past the 30 s the launch plan asks for, and each one is useful on its own (setup in the Install section, delegation at the top).

`checkout.log` is a made-up checkout-service log with three problems in it: payment timeouts for six orders, an exhausted database connection pool, and a SKU whose stock went negative. Through fm-mcp 0.1.1, `summarise` with the focus "what went wrong" as one paragraph found all three in 3 runs of 3, in about 5.5 s (2026-10-12). As bullets it did worse: in 2 runs of 3 it spent all 7 bullets on near-identical timeout lines. That's why the prompt asks for a short paragraph. fm-mcp 0.1.3 fixes the bullets: each kind of event gets one bullet, and errors come first (all three problems in 20 of 20 runs on this log without a focus, 19 of 20 with it).

## Once

```sh
brew install vhs gifsicle
```

## setup.gif

The recording installs fm-mcp for real, so remove it first. This takes fm-mcp out of Claude Code and Codex until the recording puts it back.

```sh
fm-mcp install --uninstall
brew uninstall fm-mcp
vhs docs/demo/setup.tape
```

If the recording stops early, run `brew install yrangana/tap/fm-mcp && fm-mcp install` to put everything back.

The tape tidies the screen in a hidden step:
- it drops brew's warnings, which come from the recording Mac's other taps rather than from fm-mcp;
- it shows the home folder as `~` in fm-mcp's output, so the user name stays out of the GIF;
- it clears the screen between commands.

## delegate.gif

Set up a folder with only the log in it, and open Claude Code there once by hand to answer the "trust this folder" question, so it doesn't appear in the recording:

```sh
mkdir -p /Users/Shared/fm-mcp-demo
cp docs/demo/checkout.log /Users/Shared/fm-mcp-demo/
cd /Users/Shared/fm-mcp-demo && claude    # answer the trust prompt, then /exit
cd -
vhs docs/demo/delegate.tape
```

The model's answer differs from run to run. Keep a take only if:

- the screen shows the `summarise` call going to fm-mcp (not Claude reading the file itself), and
- the answer names all three problems.

## Before using a take

- **Check every frame for personal details.** Claude Code's welcome screen can show the account's email address and organisation, and the prompt can show your user name. The tapes hide the shell prompt, but look at the first frames of `delegate.gif` in particular.
- Shrink the files: `gifsicle -O3 --lossy=80 -o docs/demo/setup.gif docs/demo/setup.gif` (and the same for `delegate.gif`). Aim for under 2 MB each, so they load quickly in the README.
- Put them in the README (`![…](docs/demo/delegate.gif)`), and copy them to `web/` for the site.
