//! The MCP server and its tools.

use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::backend::{Backend, BackendError, ChatMessage, ChatRequest};

const INSTRUCTIONS: &str = "Runs a small on-device model (for Apple Foundation Models) for free, private, offline text work: \
summarising logs, documents and notes. Not for code, maths, reasoning or facts; the model has a small context (~8K tokens) \
and can be wrong, so check anything important.";

const SUMMARISE_DESCRIPTION: &str = "Summarise text with the on-device model for Apple Foundation Models. \
Free, private and offline. Good for condensing logs, documents, notes and transcripts. \
Limits: small model with an ~8K-token context shared by input and summary, so send at most ~4,000 words; \
longer input returns an error. Not for code, maths, reasoning or facts. \
Summaries can miss or distort details; check anything important.";

// Tool schemas stay portable: one `type` per field (no `["string", "null"]`)
// and no `$ref`, because some clients reject either. A test enforces this.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SummariseParams {
    /// The text to summarise: plain text, Markdown or logs. Keep it under about 4,000 words.
    pub text: String,
    /// Optional focus, for example "errors only" or "decisions and owners". Leave empty for a general summary.
    #[serde(default)]
    pub focus: String,
    /// How long the summary should be: `short` (one or two sentences), `medium` (one paragraph of
    /// three to five sentences) or `bullets` (three to seven bullet points). Defaults to `medium`.
    #[serde(default)]
    pub length: SummaryLength,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(inline)]
pub enum SummaryLength {
    Short,
    #[default]
    Medium,
    Bullets,
}

impl SummaryLength {
    fn instruction(self) -> &'static str {
        match self {
            Self::Short => "Write one or two sentences.",
            Self::Medium => "Write one paragraph of three to five sentences.",
            Self::Bullets => {
                "Write three to seven short bullet points, one per line, each starting with \"- \"."
            }
        }
    }

    fn max_tokens(self) -> u32 {
        match self {
            Self::Short => 120,
            Self::Medium => 300,
            Self::Bullets => 350,
        }
    }
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
        annotations(
            title = "Summarise text (on-device)",
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn summarise(
        &self,
        Parameters(params): Parameters<SummariseParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.text.trim().is_empty() {
            return Ok(tool_error(
                "`text` is empty; there is nothing to summarise.",
            ));
        }
        let request = summarise_request(&params);
        Ok(match self.backend.chat(request).await {
            Ok(response) => {
                if let Some(usage) = response.usage {
                    debug!(
                        "summarise used {} prompt + {} completion tokens",
                        usage.prompt_tokens, usage.completion_tokens
                    );
                }
                match response.text().map(str::trim) {
                    Some(summary) if !summary.is_empty() => {
                        let summary = match params.length {
                            SummaryLength::Bullets => normalise_bullets(summary),
                            SummaryLength::Short | SummaryLength::Medium => summary.to_owned(),
                        };
                        CallToolResult::success(vec![ContentBlock::text(summary)])
                    }
                    _ => tool_error("The on-device model returned an empty summary."),
                }
            }
            Err(e) => {
                warn!("summarise failed: {e}");
                tool_error(&backend_error_text(&e))
            }
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

fn summarise_request(params: &SummariseParams) -> ChatRequest {
    let mut system = String::from(
        "You summarise text accurately. Use only information in the text. \
         Do not add facts, opinions or advice. ",
    );
    system.push_str(params.length.instruction());
    let focus = params.focus.trim();
    if !focus.is_empty() {
        system.push_str(&format!(
            " Focus on: {focus}. Leave out anything unrelated."
        ));
    }
    let user = format!("Summarise this text:\n\n{}", params.text);

    ChatRequest::new(vec![ChatMessage::system(system), ChatMessage::user(user)])
        .with_max_tokens(params.length.max_tokens())
}

/// One "- " bullet per non-empty line. The model sometimes doubles the marker ("- - ").
fn normalise_bullets(summary: &str) -> String {
    summary
        .lines()
        .map(|line| {
            line.trim_start_matches(|c: char| matches!(c, '-' | '*' | '•') || c.is_whitespace())
        })
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
        BackendError::Unavailable(detail) => format!(
            "The on-device model is not available: {detail}. Apple Intelligence may be off or still \
             downloading. Run `fm-mcp doctor` for details."
        ),
        BackendError::Connection(detail) => format!(
            "The on-device model server stopped responding ({detail}). Run `fm-mcp doctor`."
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

    fn params(length: SummaryLength, focus: Option<&str>) -> SummariseParams {
        SummariseParams {
            text: "Some text.".into(),
            focus: focus.unwrap_or_default().into(),
            length,
        }
    }

    fn tool_schemas() -> Vec<serde_json::Value> {
        let backend: Arc<dyn Backend> = Arc::new(NoBackend);
        FmMcp::new(backend)
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| serde_json::Value::Object((*tool.input_schema).clone()))
            .collect()
    }

    /// Collects every `type` value and `$ref` key found anywhere in a schema.
    fn walk(value: &serde_json::Value, types: &mut Vec<serde_json::Value>, refs: &mut usize) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    match key.as_str() {
                        "type" => types.push(child.clone()),
                        "$ref" => *refs += 1,
                        _ => {}
                    }
                    walk(child, types, refs);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, types, refs);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn tool_schemas_should_use_a_single_type_per_field() {
        for schema in tool_schemas() {
            let (mut types, mut refs) = (Vec::new(), 0);
            walk(&schema, &mut types, &mut refs);
            assert!(types.iter().all(serde_json::Value::is_string), "{schema}");
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
        let schema = &tool_schemas()[0];
        let length = &schema["properties"]["length"];
        assert_eq!(
            (&length["enum"], &length["default"]),
            (
                &serde_json::json!(["short", "medium", "bullets"]),
                &serde_json::json!("medium")
            )
        );
    }

    #[test]
    fn summarise_description_should_state_limits() {
        assert!(SUMMARISE_DESCRIPTION.contains("Not for code"));
    }

    #[test]
    fn summarise_description_should_stay_under_600_chars() {
        assert!(SUMMARISE_DESCRIPTION.len() < 600);
    }

    #[test]
    fn summarise_request_should_include_focus_in_system_prompt() {
        let request = summarise_request(&params(SummaryLength::Medium, Some("errors only")));
        assert!(
            request.messages[0]
                .content
                .contains("Focus on: errors only.")
        );
    }

    #[test]
    fn summarise_request_should_ignore_blank_focus() {
        let request = summarise_request(&params(SummaryLength::Medium, Some("  ")));
        assert!(!request.messages[0].content.contains("Focus on"));
    }

    #[test]
    fn summarise_request_should_cap_tokens_by_length() {
        let request = summarise_request(&params(SummaryLength::Short, None));
        assert_eq!(request.max_tokens, Some(120));
    }

    #[test]
    fn normalise_bullets_should_collapse_doubled_markers() {
        let input = "- - first\n\n- second\n* third";
        assert_eq!(normalise_bullets(input), "- first\n- second\n- third");
    }

    #[test]
    fn normalise_bullets_should_keep_hyphens_inside_text() {
        assert_eq!(normalise_bullets("- on-device model"), "- on-device model");
    }

    #[test]
    fn server_info_should_name_fm_mcp() {
        let backend: Arc<dyn Backend> = Arc::new(NoBackend);
        let info = FmMcp::new(backend).get_info();
        assert_eq!(info.server_info.name, "fm-mcp");
    }

    struct NoBackend;

    impl Backend for NoBackend {
        fn chat(
            &self,
            _request: ChatRequest,
        ) -> crate::backend::BoxFuture<'_, Result<crate::backend::ChatResponse, BackendError>>
        {
            Box::pin(async { Err(BackendError::Connection("test".into())) })
        }
    }

    #[test]
    fn length_should_default_to_medium_when_omitted() {
        let params: SummariseParams = serde_json::from_str(r#"{"text":"x"}"#).unwrap();
        assert!(matches!(params.length, SummaryLength::Medium));
    }
}
