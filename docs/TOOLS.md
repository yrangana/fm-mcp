# fm-mcp tools

The four tools fm-mcp gives an agent, with every parameter. The descriptions below are the ones the server sends, so this is exactly what the agent reads. The [README](../README.md#limits) has the limits in plain terms.

All four run on the on-device model, one request at a time: the model is shared by every session on the Mac, so calls queue. None of them writes files or uses the network.

<!-- tools:start (generated from tools/list by a test; run `UPDATE_DOCS=1 cargo test --features fake-fm tools_md` to refresh) -->

## `summarise`: Summarise text (on-device)

Summarise text with the on-device model for Apple Foundation Models. Free, private and offline. Good for condensing logs, documents, notes and transcripts into a gist (a paragraph, or at most 7 bullets), not a full record. Limit: about 30,000 words of prose but only about 1,000 lines of a dense log; filter bigger logs first (grep the errors). Pass `path` to a text file instead of `text` to avoid reading it yourself. Long input is split and combined: slower, and it can drop details. Not for code, maths, reasoning or facts. Summaries can miss or distort details; check anything important.

| Parameter | Type | Required | Default | What it is |
|---|---|---|---|---|
| `text` | string | no | empty | The text to summarise: plain text, Markdown or logs. Give this or `path`. |
| `path` | string | no | empty | Absolute path to a UTF-8 text file to summarise instead of `text`. |
| `focus` | string | no | empty | Optional focus, for example "errors only" or "decisions and owners". Leave empty for a general summary. |
| `length` | string: `short`, `medium`, `bullets` | no | `medium` | How long the summary should be: `short` (one or two sentences), `medium` (one paragraph of three to five sentences) or `bullets` (three to seven bullet points). Defaults to `medium`. |

## `extract`: Extract fields as JSON (on-device)

Extract fields from text into JSON matching a JSON Schema, with the on-device model for Apple Foundation Models (free, private, offline). Good for names, dates, amounts and IDs in short documents, emails or logs. Input up to about 3,500 words; pass `path` to a text file instead of `text` so you don't have to read it first. Fields the text lacks usually come back as null. Flat schemas work best; nested objects are unreliable. Allowed keywords: type, properties, required, items, enum, const, description, minItems, maxItems. Not for code or reasoning. Values can be wrong; check what matters.

| Parameter | Type | Required | Default | What it is |
|---|---|---|---|---|
| `text` | string | no | empty | The text to extract from. Up to about 3,500 words. Give this or `path`. |
| `path` | string | no | empty | Absolute path to a UTF-8 text file to extract from instead of `text`. |
| `schema` | object | yes |  | A JSON Schema for the result. The top level must be an object, for example {"type": "object", "properties": {"invoice": {"type": "string"}, "total": {"type": "number"}}}. |
| `instructions` | string | no | empty | Optional extra guidance, for example "amounts in AUD without the currency sign". |

## `classify`: Classify text (on-device)

Pick the best label for a text from a list you give, with the on-device model for Apple Foundation Models (free, private, offline). Good for triage: sorting tickets, emails, comments or log lines into categories. The answer is always one of your labels (or several, with `multi`). Input up to about 3,500 words, as `text` or as `path` to a text file; 2 to 50 labels. Subtle or ambiguous text can be mislabelled, so spot-check. Not for code, maths or reasoning.

| Parameter | Type | Required | Default | What it is |
|---|---|---|---|---|
| `text` | string | no | empty | The text to classify. Up to about 3,500 words. Give this or `path`. |
| `path` | string | no | empty | Absolute path to a UTF-8 text file to classify (as one text) instead of `text`. |
| `labels` | array of string | yes |  | The labels to choose from: 2 to 50 distinct, non-empty strings. |
| `multi` | boolean | no | `false` | Allow several labels instead of exactly one. Defaults to false. |
| `instructions` | string | no | empty | Optional guidance, for example what each label means. |

## `ocr`: Read text in an image (on-device)

Read the text in an image file (screenshot, scanned page, receipt, handwriting) with the on-device model for Apple Foundation Models (free, private, offline). Give an absolute path to a PNG, JPEG, HEIC, TIFF, GIF or BMP file; PDFs are not supported. Returns the text line by line, or answers `prompt` about the image instead. Small or unusual text can be misread; check numbers that matter. Not for code review or reasoning.

| Parameter | Type | Required | Default | What it is |
|---|---|---|---|---|
| `path` | string | yes |  | Absolute path to a PNG, JPEG, HEIC, TIFF, GIF or BMP image. |
| `prompt` | string | no | empty | Optional question or instruction about the image. Leave empty to get all the text. |

<!-- tools:end -->

## Example calls

The arguments an agent sends, as JSON. Each was run against the real model on macOS 27.0.1 with fm-mcp 0.1.0.

**`summarise`** a log file, errors only, as bullets:

```json
{"path": "/Users/me/logs/build.log", "focus": "errors only", "length": "bullets"}
```

**`extract`** an invoice's fields. For amounts written with a currency sign or commas (`$1,280.00`), say how to write them in `instructions`; without that, a `number` field often comes back `null`. Ask only for fields the text is likely to have: a missing field can come back as `null`, as a wrong value, or as the "got stuck" error:

```json
{
  "text": "Invoice INV-2041 from Northwind Traders, dated 3 March 2026. Total due: $1,280.00 by 2 April 2026.",
  "instructions": "Write amounts as plain numbers, without currency signs or commas.",
  "schema": {
    "type": "object",
    "properties": {
      "invoice_number": {"type": "string"},
      "supplier": {"type": "string"},
      "total": {"type": "number"},
      "due_date": {"type": "string"}
    }
  }
}
```

**`classify`** a support message into one label:

```json
{
  "text": "I was charged twice for my subscription this month.",
  "labels": ["billing", "bug", "feature request", "account access"]
}
```

**`ocr`** a screenshot, or ask about it with `prompt`:

```json
{"path": "/Users/me/Desktop/receipt.png"}
```

```json
{"path": "/Users/me/Desktop/receipt.png", "prompt": "What is the total?"}
```
