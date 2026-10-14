# fm-mcp plugin for Claude Code

Lets Claude Code hand simple text work to the on-device model for Apple Foundation Models, the `fm` tool that ships with macOS 27: summarising logs and documents, pulling fields out of text as JSON, sorting text into your labels, and reading the text in images. It's free, private and works offline.

The plugin adds two things:

- the **fm-mcp MCP server** (`summarise`, `extract`, `classify`, `ocr`), started from the `fm-mcp` command;
- the **fm-delegate skill**, which tells Claude when to delegate to the small model and when to keep the work.

## Install the binary first

The plugin runs the `fm-mcp` binary, which it doesn't include. Install it with Homebrew, then check your Mac:

```sh
brew install yrangana/tap/fm-mcp
fm-mcp doctor
```

You need an Apple Silicon Mac with macOS 27, Apple Intelligence turned on, and the `fm` licence accepted once (`fm license`). Intel Macs, Windows and Linux are not supported.

Then add the plugin in Claude Code (2.1.275 or later):

```
/plugin install fm-mcp --marketplace yrangana/fm-mcp
```

## Limits

The model is small: about 8,000 tokens of context, one request at a time for the whole Mac, and it makes mistakes. Each tool's description states its limits, so Claude reads them before every call.

More: [README](https://github.com/yrangana/fm-mcp#readme) · [site](https://yrangana.github.io/fm-mcp/) · [tools](https://github.com/yrangana/fm-mcp/blob/main/docs/TOOLS.md). MIT licence. Not affiliated with or endorsed by Apple.
