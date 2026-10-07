//! Model backend abstraction.
//!
//! Every backend speaks the Chat Completions format, so Ollama or Foundry Local
//! can be added later behind the same trait. v1 ships only the `fm serve` backend.

use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A model that answers Chat Completions requests.
pub trait Backend: Send + Sync {
    fn chat(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatResponse, BackendError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }
}

/// A non-streaming chat request.
///
/// `stream` is private and always `false`: `fm serve` streams by default
/// unless told otherwise (see AGENTS.md verified facts).
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    model: &'static str,
    stream: bool,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

impl ChatRequest {
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            model: "system",
            stream: false,
            messages,
            max_tokens: None,
        }
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    pub message: ResponseMessage,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseMessage {
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

impl ChatResponse {
    /// The text of the first choice, if any.
    pub fn text(&self) -> Option<&str> {
        self.choices.first()?.message.content.as_deref()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("input is too long for the model's context")]
    ContextOverflow,
    #[error("the model's safety guardrails were triggered")]
    Guardrail,
    #[error("model server returned HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("the model is not available: {0}")]
    Unavailable(String),
    #[error("could not reach the model server: {0}")]
    Connection(String),
    #[error("the model did not answer within {0} s")]
    Timeout(u64),
    #[error("unexpected response from the model server: {0}")]
    BadResponse(String),
}

impl BackendError {
    /// Classify a non-2xx response from a Chat Completions server.
    pub fn from_http(status: u16, body: &[u8]) -> Self {
        #[derive(Deserialize)]
        struct ErrorBody {
            error: ErrorDetail,
        }
        #[derive(Deserialize)]
        struct ErrorDetail {
            message: String,
        }

        let message = serde_json::from_slice::<ErrorBody>(body)
            .map(|b| b.error.message)
            .unwrap_or_else(|_| String::from_utf8_lossy(body).into_owned());

        if message.contains("exceeded the model's context size") {
            Self::ContextOverflow
        } else if message.contains("safety guardrails were triggered") {
            Self::Guardrail
        } else {
            Self::Http { status, message }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_should_always_serialise_stream_false() {
        let request = ChatRequest::new(vec![ChatMessage::user("hi")]);
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["stream"], serde_json::Value::Bool(false));
    }

    #[test]
    fn request_should_omit_max_tokens_when_unset() {
        let request = ChatRequest::new(vec![ChatMessage::user("hi")]);
        let json = serde_json::to_value(&request).unwrap();
        assert!(json.get("max_tokens").is_none());
    }

    #[test]
    fn from_http_should_detect_context_overflow() {
        let body = br#"{"error":{"code":"500","message":"The session's transcript exceeded the model's context size.","type":"server_error"}}"#;
        assert!(matches!(
            BackendError::from_http(500, body),
            BackendError::ContextOverflow
        ));
    }

    #[test]
    fn from_http_should_detect_guardrail() {
        let body = br#"{"error":{"code":"500","message":"The model's safety guardrails were triggered.","type":"server_error"}}"#;
        assert!(matches!(
            BackendError::from_http(500, body),
            BackendError::Guardrail
        ));
    }

    #[test]
    fn from_http_should_keep_message_for_other_errors() {
        let body =
            br#"{"error":{"code":"400","message":"Invalid JSON","type":"invalid_request_error"}}"#;
        let BackendError::Http { status, message } = BackendError::from_http(400, body) else {
            panic!("expected Http error");
        };
        assert_eq!((status, message.as_str()), (400, "Invalid JSON"));
    }

    #[test]
    fn response_text_should_return_first_choice_content() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"Hello."},"index":0}],"usage":{"prompt_tokens":61,"completion_tokens":3}}"#;
        let response: ChatResponse = serde_json::from_str(body).unwrap();
        assert_eq!(response.text(), Some("Hello."));
    }
}
