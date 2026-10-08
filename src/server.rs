//! The MCP server and its tools.

use std::{path::Path, sync::Arc, time::Duration};

use base64::Engine;
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, Implementation, ProgressNotificationParam, ProgressToken,
        ServerCapabilities, ServerConfig,
    },
    schemars,
    service::{Peer, RequestContext},
    tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::warn;

use crate::{
    backend::{Backend, BackendError, BoxFuture, ChatMessage, ChatRequest},
    chunk, schema,
    summarise::{self, NoProgress, Progress, SummariseError, SummaryLength},
};

const INSTRUCTIONS: &str = "Runs a small on-device model (for Apple Foundation Models) for free, \
private, offline text work: summarising, extracting fields, classifying, and reading text in \
images. Not for code, maths, reasoning or facts; the model has a small context (~8K tokens per \
call) and can be wrong, so check anything important.";

const SUMMARISE_DESCRIPTION: &str = "Summarise text with the on-device model for Apple \
Foundation Models. Free, private and offline. Good for condensing logs, documents, notes and \
transcripts. Limit: about 30,000 words of prose but only about 1,000 lines of a dense log; \
filter bigger logs first (for example, grep the errors). Long input is split into parts and \
combined, which is slower and can drop details. Not for code, maths, reasoning or facts. \
Summaries can miss or distort details; check anything important.";

const EXTRACT_DESCRIPTION: &str = "Extract fields from text into JSON matching a JSON Schema, \
with the on-device model for Apple Foundation Models (free, private, offline). Good for names, \
dates, amounts and IDs in short documents, emails or logs. Input up to about 3,500 words. Fields \
the text lacks usually come back as null. Flat schemas work best; nested objects are unreliable. Allowed \
keywords: type, properties, required, items, enum, const, description, minItems, maxItems. Not \
for code or reasoning. Values can be wrong; check what matters.";

const CLASSIFY_DESCRIPTION: &str = "Pick the best label for a text from a list you give, with \
the on-device model for Apple Foundation Models (free, private, offline). Good for triage: \
sorting tickets, emails, comments or log lines into categories. The answer is always one of \
your labels (or several, with `multi`). Input up to about 3,500 words; 2 to 50 labels. Subtle \
or ambiguous text can be mislabelled, so spot-check. Not for code, maths or reasoning.";

const OCR_DESCRIPTION: &str = "Read the text in an image file (screenshot, scanned page, \
receipt, handwriting) with the on-device model for Apple Foundation Models (free, private, \
offline). Give an absolute path to a PNG, JPEG, HEIC, TIFF, GIF or BMP file; PDFs are not \
supported. Returns the text line by line, or answers `prompt` about the image instead. Small \
or unusual text can be misread; check numbers that matter. Not for code review or reasoning.";

/// Structured calls normally take 1–2 s; a runaway would take minutes.
const STRUCTURED_TIMEOUT: Duration = Duration::from_secs(30);
/// Tokens set aside for instructions in `extract` and `classify` calls.
const STRUCTURED_INSTRUCTION_TOKENS: usize = 200;
const EXTRACT_OUTPUT_TOKENS: u32 = 1000;
/// A 50-label `multi` answer measured 394 tokens (2026-10-07).
const CLASSIFY_OUTPUT_TOKENS: u32 = 500;
const MAX_LABELS: usize = 50;
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const OCR_PROMPT: &str =
    "Return all the text in this image exactly as written, line by line. Do not add anything.";

// Tool schemas stay portable: one `type` per field (no `["string", "null"]`)
// and no `$ref`, because some clients reject either. A test enforces this.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SummariseParams {
    /// The text to summarise: plain text, Markdown or logs.
    pub text: String,
    /// Optional focus, for example "errors only" or "decisions and owners". Leave empty for a general summary.
    #[serde(default)]
    pub focus: String,
    /// How long the summary should be: `short` (one or two sentences), `medium` (one paragraph of
    /// three to five sentences) or `bullets` (three to seven bullet points). Defaults to `medium`.
    #[serde(default)]
    pub length: SummaryLength,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExtractParams {
    /// The text to extract from. Up to about 3,500 words.
    pub text: String,
    /// A JSON Schema for the result. The top level must be an object, for example
    /// {"type": "object", "properties": {"invoice": {"type": "string"}, "total": {"type": "number"}}}.
    pub schema: serde_json::Map<String, Value>,
    /// Optional extra guidance, for example "amounts in AUD without the currency sign".
    #[serde(default)]
    pub instructions: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClassifyParams {
    /// The text to classify. Up to about 3,500 words.
    pub text: String,
    /// The labels to choose from: 2 to 50 distinct, non-empty strings.
    pub labels: Vec<String>,
    /// Allow several labels instead of exactly one. Defaults to false.
    #[serde(default)]
    pub multi: bool,
    /// Optional guidance, for example what each label means.
    #[serde(default)]
    pub instructions: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OcrParams {
    /// Absolute path to a PNG, JPEG, HEIC, TIFF, GIF or BMP image.
    pub path: String,
    /// Optional question or instruction about the image. Leave empty to get all the text.
    #[serde(default)]
    pub prompt: String,
}

#[derive(Clone)]
pub struct FmMcp {
    backend: Arc<dyn Backend>,
    tool_router: ToolRouter<FmMcp>,
}

#[tool_router]
impl FmMcp {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = SUMMARISE_DESCRIPTION,
        annotations(title = "Summarise text (on-device)", read_only_hint = true, destructive_hint = false, open_world_hint = false)
    )]
    async fn summarise(
        &self,
        Parameters(params): Parameters<SummariseParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        if params.text.trim().is_empty() {
            return Ok(tool_error(
                "`text` is empty; there is nothing to summarise.",
            ));
        }
        let progress: Box<dyn Progress> = match context.meta.get_progress_token() {
            Some(token) => Box::new(PeerProgress {
                peer: context.peer.clone(),
                token,
            }),
            None => Box::new(NoProgress),
        };
        let result = summarise::summarise(
            self.backend.as_ref(),
            &params.text,
            &params.focus,
            params.length,
            progress.as_ref(),
        )
        .await;
        Ok(match result {
            Ok(summary) => {
                let mut text = match params.length {
                    SummaryLength::Bullets => normalise_bullets(&summary.text),
                    SummaryLength::Short | SummaryLength::Medium => summary.text,
                };
                if summary.parts > 1 {
                    text.push_str(&format!(
                        "\n\n(Long input: summarised in {} parts, then combined.)",
                        summary.parts
                    ));
                }
                CallToolResult::success(vec![ContentBlock::text(text)])
            }
            Err(SummariseError::TooLong(tokens)) => tool_error(&too_long_for_summarise(tokens)),
            Err(SummariseError::Backend(e)) => {
                warn!("summarise failed: {e}");
                tool_error(&backend_error_text(&e))
            }
        })
    }

    #[tool(
        description = EXTRACT_DESCRIPTION,
        annotations(title = "Extract fields as JSON (on-device)", read_only_hint = true, destructive_hint = false, open_world_hint = false)
    )]
    async fn extract(
        &self,
        Parameters(params): Parameters<ExtractParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.text.trim().is_empty() {
            return Ok(tool_error(
                "`text` is empty; there is nothing to extract from.",
            ));
        }
        let schema = match schema::prepare(&Value::Object(params.schema)) {
            Ok(schema) => schema,
            Err(problem) => return Ok(tool_error(&format!("Unusable schema: {problem}."))),
        };
        if let Err(message) = self.check_fits(&params.text, EXTRACT_OUTPUT_TOKENS).await {
            return Ok(tool_error(&message));
        }
        let mut system = String::from(
            "Extract the requested fields from the text. Copy values exactly as written. \
             Use null for anything the text does not state; never guess. Most of the text \
             belongs to no field: leave it out. A field gets a value only when the text \
             states that exact thing; otherwise it is null.",
        );
        push_guidance(&mut system, &params.instructions);
        let request = ChatRequest::new(vec![
            ChatMessage::system(system),
            ChatMessage::user(params.text),
        ])
        .with_json_schema("Extraction", schema.clone())
        .with_max_tokens(EXTRACT_OUTPUT_TOKENS)
        .with_timeout(STRUCTURED_TIMEOUT);
        Ok(self
            .structured_call(request, &schema, "extract")
            .await
            .map_or_else(|error| error, CallToolResult::structured))
    }

    #[tool(
        description = CLASSIFY_DESCRIPTION,
        annotations(title = "Classify text (on-device)", read_only_hint = true, destructive_hint = false, open_world_hint = false)
    )]
    async fn classify(
        &self,
        Parameters(params): Parameters<ClassifyParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.text.trim().is_empty() {
            return Ok(tool_error("`text` is empty; there is nothing to classify."));
        }
        if let Err(problem) = check_labels(&params.labels) {
            return Ok(tool_error(&problem));
        }
        if let Err(message) = self.check_fits(&params.text, CLASSIFY_OUTPUT_TOKENS).await {
            return Ok(tool_error(&message));
        }
        let labels = json!(params.labels);
        let schema = if params.multi {
            json!({"type": "object", "properties": {"labels": {
                "type": "array", "items": {"type": "string", "enum": labels},
                "minItems": 1, "maxItems": params.labels.len()}}, "required": ["labels"]})
        } else {
            json!({"type": "object", "properties": {"label": {"type": "string", "enum": labels}},
                   "required": ["label"]})
        };
        let mut system = if params.multi {
            String::from("Choose every label that applies to the text. Use only the given labels.")
        } else {
            String::from(
                "Choose the single label that best fits the text. Use only the given labels.",
            )
        };
        push_guidance(&mut system, &params.instructions);
        let request = ChatRequest::new(vec![
            ChatMessage::system(system),
            ChatMessage::user(params.text),
        ])
        .with_json_schema("Classification", schema.clone())
        .with_max_tokens(CLASSIFY_OUTPUT_TOKENS)
        .with_timeout(STRUCTURED_TIMEOUT);
        Ok(
            match self.structured_call(request, &schema, "classify").await {
                // The model can repeat a label in a `multi` answer (seen 2026-10-08).
                Ok(mut value) => {
                    if let Some(chosen) = value.get_mut("labels").and_then(Value::as_array_mut) {
                        let mut seen = std::collections::HashSet::new();
                        chosen.retain(|label| seen.insert(label.clone()));
                    }
                    CallToolResult::structured(value)
                }
                Err(error) => error,
            },
        )
    }

    #[tool(
        description = OCR_DESCRIPTION,
        annotations(title = "Read text in an image (on-device)", read_only_hint = true, destructive_hint = false, open_world_hint = false)
    )]
    async fn ocr(
        &self,
        Parameters(params): Parameters<OcrParams>,
    ) -> Result<CallToolResult, McpError> {
        let data_url = match read_image(Path::new(&params.path)).await {
            Ok(url) => url,
            Err(problem) => return Ok(tool_error(&problem)),
        };
        let prompt = if params.prompt.trim().is_empty() {
            OCR_PROMPT.to_owned()
        } else {
            params.prompt
        };
        let request = ChatRequest::new(vec![ChatMessage::user_with_image(prompt, data_url)]);
        Ok(match self.backend.chat(request).await {
            Ok(response) => match response.text().map(str::trim) {
                Some(text) if !text.is_empty() => {
                    CallToolResult::success(vec![ContentBlock::text(text)])
                }
                _ => tool_error("The on-device model found no text in the image."),
            },
            Err(e) => {
                warn!("ocr failed: {e}");
                tool_error(&backend_error_text(&e))
            }
        })
    }
}

impl FmMcp {
    /// Refuses input too long for one call (D2: only `summarise` splits input).
    async fn check_fits(&self, text: &str, output_tokens: u32) -> Result<(), String> {
        let budget = chunk::input_budget(STRUCTURED_INSTRUCTION_TOKENS, output_tokens as usize);
        let too_long = |tokens: String| {
            format!(
                "Input is too long for the on-device model ({tokens}; the limit for this tool is \
                 about {budget} tokens). Send only the part that matters."
            )
        };
        if text.chars().count() > budget * chunk::MAX_CHARS_PER_TOKEN {
            return Err(too_long("far over the limit".into()));
        }
        match self.backend.count_tokens(text).await {
            Ok(tokens) if tokens > budget => Err(too_long(format!("{tokens} tokens"))),
            Ok(_) => Ok(()),
            Err(e) => Err(backend_error_text(&e)),
        }
    }

    /// Sends a structured-output request and checks the answer against `schema`.
    /// The error is the tool result to return as it is.
    async fn structured_call(
        &self,
        request: ChatRequest,
        schema: &Value,
        tool: &str,
    ) -> Result<Value, CallToolResult> {
        let response = match self.backend.chat(request).await {
            Ok(response) => response,
            Err(BackendError::Timeout(secs)) => {
                warn!("{tool}: structured output ran away; fm serve replaced");
                return Err(tool_error(&format!(
                    "The on-device model got stuck and was stopped after {secs} s. Either the \
                     schema made it run away (nested, or many fields the text doesn't contain), \
                     or another session is using the model. Try once more with a flatter schema \
                     and fewer fields; if that fails too, do this task yourself."
                )));
            }
            Err(e) => {
                warn!("{tool} failed: {e}");
                return Err(tool_error(&backend_error_text(&e)));
            }
        };
        let text = response.text().unwrap_or_default();
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(_) => {
                return Err(tool_error(
                    "The on-device model returned invalid JSON. Try again, or do this task yourself.",
                ));
            }
        };
        schema::validate(&value, schema).map(|()| value).map_err(|problem| {
            tool_error(&format!(
                "The on-device model's answer didn't match the schema ({problem}). Try again with \
                 a simpler schema, or do this task yourself."
            ))
        })
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for FmMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(INSTRUCTIONS)
    }
}

/// Sends MCP progress notifications for a long `summarise`.
struct PeerProgress {
    peer: Peer<RoleServer>,
    token: ProgressToken,
}

impl Progress for PeerProgress {
    fn report(&self, done: usize, total: usize, message: String) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let param = ProgressNotificationParam::new(self.token.clone(), done as f64)
                .with_total(total as f64)
                .with_message(message);
            if let Err(e) = self.peer.notify_progress(param).await {
                warn!("could not send progress: {e}");
            }
        })
    }
}

fn check_labels(labels: &[String]) -> Result<(), String> {
    if !(2..=MAX_LABELS).contains(&labels.len()) {
        return Err(format!(
            "Give between 2 and {MAX_LABELS} labels (got {}).",
            labels.len()
        ));
    }
    if labels.iter().any(|l| l.trim().is_empty()) {
        return Err("Labels must not be empty.".into());
    }
    let mut seen = std::collections::HashSet::new();
    if let Some(duplicate) = labels.iter().find(|l| !seen.insert(l.as_str())) {
        return Err(format!(
            "Labels must be distinct; `{duplicate}` appears twice."
        ));
    }
    Ok(())
}

/// Reads an image file into a data URL, after checking it's a supported type.
async fn read_image(path: &Path) -> Result<String, String> {
    if !path.is_absolute() {
        return Err(format!(
            "`path` must be absolute (got `{}`).",
            path.display()
        ));
    }
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let mime = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "heic" => "image/heic",
        "tif" | "tiff" => "image/tiff",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "pdf" => return Err("PDFs are not supported. Convert the page to PNG first (for example with `sips -s format png`).".into()),
        _ => return Err(format!("Unsupported file type `.{extension}`. Use PNG, JPEG, HEIC, TIFF, GIF or BMP.")),
    };
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|e| format!("Cannot read `{}`: {e}.", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("`{}` is not a file.", path.display()));
    }
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "`{}` is larger than 20 MB; scale it down first.",
            path.display()
        ));
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("Cannot read `{}`: {e}.", path.display()))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

fn push_guidance(system: &mut String, guidance: &str) {
    let guidance = guidance.trim();
    if !guidance.is_empty() {
        system.push_str(&format!(" Extra guidance: {guidance}"));
    }
}

fn too_long_for_summarise(tokens: Option<usize>) -> String {
    let size = tokens.map_or_else(
        || "far over the limit".to_owned(),
        |t| format!("{t} tokens"),
    );
    format!(
        "Input is too long to summarise ({size}; the limit is {} tokens, about 30,000 words of \
         prose or much less for dense logs). Send less, for example the most recent or most \
         relevant section.",
        chunk::MAX_INPUT_TOKENS
    )
}

/// One "- " bullet per non-empty line. The model sometimes doubles the marker
/// ("- - "), so up to two markers are removed; a third `-` is kept, since it may
/// be a minus sign ("- -5 °C" stays "- -5 °C").
fn normalise_bullets(summary: &str) -> String {
    fn strip_marker(line: &str) -> &str {
        let line = line.trim_start();
        match line.strip_prefix(['-', '*', '•']) {
            Some(rest) if rest.is_empty() || rest.starts_with(char::is_whitespace) => {
                rest.trim_start()
            }
            _ => line,
        }
    }
    summary
        .lines()
        .map(|line| strip_marker(strip_marker(line)))
        .filter(|line| !line.is_empty())
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_error(message: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// Turns a backend failure into a message that tells the agent what to do next.
fn backend_error_text(error: &BackendError) -> String {
    match error {
        BackendError::ContextOverflow => {
            "Input is too long for the on-device model (about 8K tokens including the reply). \
             Send a shorter excerpt, or split it and call this tool once per part."
                .into()
        }
        BackendError::Guardrail => {
            "The on-device model's safety filter refused this input. This often happens with \
             harmless text. Do this task yourself instead of retrying."
                .into()
        }
        BackendError::Http { status, message } => format!(
            "fm-mcp sent a request the model server rejected (HTTP {status}: {message}). \
             This is a bug in fm-mcp; please report it."
        ),
        BackendError::ModelUnavailable => "Apple Intelligence is not available on this Mac \
             (it may be turned off or still downloading). Run `fm-mcp doctor` for details. \
             Do this task yourself for now."
            .into(),
        BackendError::StartFailed(detail) => format!(
            "The on-device model server could not start: {detail}. Run `fm-mcp doctor` for details. \
             Do this task yourself for now."
        ),
        // The detail (e.g. "connection refused") is logged, not shown: it doesn't help the agent.
        BackendError::Connection(_) => "The on-device model server stopped responding. \
             Run `fm-mcp doctor`. Do this task yourself for now."
            .into(),
        BackendError::CrashLoop(crashes) => format!(
            "The on-device model server keeps crashing ({crashes} times in the last minute), so \
             fm-mcp has stopped restarting it for now. Run `fm-mcp doctor`. Do this task yourself."
        ),
        BackendError::CountFailed(detail) => format!(
            "fm-mcp could not measure the input's size with `fm count-tokens` ({detail}). \
             Run `fm-mcp doctor`. Do this task yourself for now."
        ),
        BackendError::Timeout(secs) => format!(
            "The on-device model took longer than {secs} s. Other sessions may be using it; \
             try a shorter input."
        ),
        BackendError::BadResponse(detail) => format!(
            "The model server sent an unexpected response ({detail}). This is a bug in fm-mcp; \
             please report it."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ChatResponse;

    const DESCRIPTIONS: [(&str, &str); 4] = [
        ("summarise", SUMMARISE_DESCRIPTION),
        ("extract", EXTRACT_DESCRIPTION),
        ("classify", CLASSIFY_DESCRIPTION),
        ("ocr", OCR_DESCRIPTION),
    ];

    struct NoBackend;

    impl Backend for NoBackend {
        fn chat(&self, _request: ChatRequest) -> BoxFuture<'_, Result<ChatResponse, BackendError>> {
            Box::pin(async { Err(BackendError::Connection("test".into())) })
        }

        fn count_tokens<'a>(
            &'a self,
            _text: &'a str,
        ) -> BoxFuture<'a, Result<usize, BackendError>> {
            Box::pin(async { Ok(1) })
        }
    }

    fn server() -> FmMcp {
        FmMcp::new(Arc::new(NoBackend))
    }

    fn tool_schemas() -> Vec<Value> {
        server()
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| Value::Object((*tool.input_schema).clone()))
            .collect()
    }

    /// Collects every `type` value and counts `$ref` keys anywhere in a schema.
    fn walk(value: &Value, types: &mut Vec<Value>, refs: &mut usize) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    match key.as_str() {
                        "type" => types.push(child.clone()),
                        "$ref" => *refs += 1,
                        _ => {}
                    }
                    walk(child, types, refs);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| walk(item, types, refs)),
            _ => {}
        }
    }

    #[test]
    fn server_should_list_all_four_tools() {
        let mut names: Vec<String> = server()
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["classify", "extract", "ocr", "summarise"]);
    }

    #[test]
    fn every_description_should_state_it_is_not_for_code() {
        for (name, description) in DESCRIPTIONS {
            assert!(description.contains("Not for code"), "{name}");
        }
    }

    #[test]
    fn every_description_should_stay_under_600_chars() {
        for (name, description) in DESCRIPTIONS {
            assert!(description.len() < 600, "{name}: {}", description.len());
        }
    }

    #[test]
    fn tool_schemas_should_use_a_single_type_per_field() {
        for schema in tool_schemas() {
            let (mut types, mut refs) = (Vec::new(), 0);
            walk(&schema, &mut types, &mut refs);
            assert!(types.iter().all(Value::is_string), "{schema}");
        }
    }

    #[test]
    fn tool_schemas_should_not_use_refs() {
        for schema in tool_schemas() {
            let (mut types, mut refs) = (Vec::new(), 0);
            walk(&schema, &mut types, &mut refs);
            assert_eq!(refs, 0, "{schema}");
        }
    }

    #[test]
    fn length_schema_should_be_an_inline_enum_with_default() {
        let summarise = tool_schemas()
            .into_iter()
            .find(|s| s["properties"].get("length").is_some())
            .unwrap();
        let length = &summarise["properties"]["length"];
        assert_eq!(
            (&length["enum"], &length["default"]),
            (&json!(["short", "medium", "bullets"]), &json!("medium"))
        );
    }

    #[test]
    fn server_info_should_name_fm_mcp() {
        assert_eq!(server().get_info().server_info.name, "fm-mcp");
    }

    #[test]
    fn normalise_bullets_should_collapse_doubled_markers() {
        assert_eq!(
            normalise_bullets("- - first\n\n- second\n* third"),
            "- first\n- second\n- third"
        );
    }

    #[test]
    fn normalise_bullets_should_keep_a_leading_minus_sign() {
        assert_eq!(normalise_bullets("- -5 °C overnight"), "- -5 °C overnight");
    }

    #[test]
    fn normalise_bullets_should_keep_hyphens_inside_text() {
        assert_eq!(normalise_bullets("- on-device model"), "- on-device model");
    }

    #[test]
    fn check_labels_should_reject_too_few() {
        assert!(check_labels(&["only".into()]).is_err());
    }

    #[test]
    fn check_labels_should_reject_duplicates() {
        assert_eq!(
            check_labels(&["bug".into(), "bug".into()]).unwrap_err(),
            "Labels must be distinct; `bug` appears twice."
        );
    }

    #[test]
    fn read_image_should_reject_relative_paths() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert!(
            runtime
                .block_on(read_image(Path::new("shot.png")))
                .unwrap_err()
                .contains("absolute")
        );
    }

    #[test]
    fn read_image_should_reject_pdfs_with_advice() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert!(
            runtime
                .block_on(read_image(Path::new("/tmp/page.pdf")))
                .unwrap_err()
                .starts_with("PDFs are not supported")
        );
    }
}
