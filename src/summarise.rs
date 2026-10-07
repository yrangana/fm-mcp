//! `summarise`, including long input: text over one call's budget is split
//! into parts, each part is summarised, and the part summaries are combined.

use rmcp::schemars;
use serde::{Deserialize, Serialize};

use crate::{
    backend::{Backend, BackendError, BoxFuture, ChatMessage, ChatRequest},
    chunk,
};

/// Tokens set aside for the instructions in each call (they are ~60–110).
const INSTRUCTION_TOKENS: usize = 160;
/// Answer size for each part's summary.
const PART_SUMMARY_TOKENS: u32 = 400;
/// Combining rounds before giving up (each shrinks the text several-fold).
const MAX_ROUNDS: usize = 3;

/// Rules that apply to every summary. The second sentence is the R12 fix:
/// the model reversed facts and dropped key points in early tests.
const RULES: &str = "You summarise text accurately. Use only information in the text; \
do not add facts, opinions or advice. Copy names, numbers, dates, identifiers and error \
messages exactly. Keep conditions and negations (\"not\", \"only if\") as written.";

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

    pub fn max_tokens(self) -> u32 {
        match self {
            Self::Short => 120,
            Self::Medium => 300,
            Self::Bullets => 350,
        }
    }
}

pub struct Summary {
    pub text: String,
    /// How many parts the input was split into (1 = no splitting).
    pub parts: usize,
}

pub enum SummariseError {
    /// Over `chunk::MAX_INPUT_TOKENS`; `None` when refused by length alone.
    TooLong(Option<usize>),
    Backend(BackendError),
}

impl From<BackendError> for SummariseError {
    fn from(e: BackendError) -> Self {
        Self::Backend(e)
    }
}

/// Reports progress while a long input is processed.
pub trait Progress: Send + Sync {
    fn report(&self, done: usize, total: usize, message: String) -> BoxFuture<'_, ()>;
}

/// Used when the client didn't ask for progress updates.
pub struct NoProgress;

impl Progress for NoProgress {
    fn report(&self, _: usize, _: usize, _: String) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

pub async fn summarise(
    backend: &dyn Backend,
    text: &str,
    focus: &str,
    length: SummaryLength,
    progress: &dyn Progress,
) -> Result<Summary, SummariseError> {
    if chunk::certainly_over_cap(text) {
        return Err(SummariseError::TooLong(None));
    }
    let tokens = backend.count_tokens(text).await?;
    if tokens > chunk::MAX_INPUT_TOKENS {
        return Err(SummariseError::TooLong(Some(tokens)));
    }

    let budget = chunk::input_budget(INSTRUCTION_TOKENS, length.max_tokens() as usize);
    if tokens <= budget {
        let text = call(backend, final_request(text, focus, length, false)).await?;
        return Ok(Summary { text, parts: 1 });
    }

    // Long input: summarise parts, then combine, until it fits in one call.
    let mut current = text.to_owned();
    let mut current_tokens = tokens;
    let mut parts = 0;
    for _ in 0..MAX_ROUNDS {
        let pieces = split_to_fit(backend, &current, current_tokens).await?;
        if parts == 0 {
            parts = pieces.len();
        }
        let total = pieces.len() + 1;
        let mut summaries = Vec::with_capacity(pieces.len());
        for (i, piece) in pieces.iter().enumerate() {
            progress
                .report(
                    i,
                    total,
                    format!("Summarising part {} of {}", i + 1, pieces.len()),
                )
                .await;
            summaries.push(call(backend, part_request(piece, i + 1, pieces.len(), focus)).await?);
        }
        current = summaries
            .iter()
            .enumerate()
            .map(|(i, s)| format!("Part {}:\n{s}", i + 1))
            .collect::<Vec<_>>()
            .join("\n\n");
        current_tokens = backend.count_tokens(&current).await?;
        if current_tokens <= budget {
            progress
                .report(total - 1, total, "Combining the parts".into())
                .await;
            let text = call(backend, final_request(&current, focus, length, true)).await?;
            return Ok(Summary { text, parts });
        }
    }
    Err(SummariseError::TooLong(Some(tokens)))
}

/// Splits `text` into pieces that each fit a part-summary call, checked with
/// real token counts (text density varies too much to trust an estimate).
async fn split_to_fit(
    backend: &dyn Backend,
    text: &str,
    tokens: usize,
) -> Result<Vec<String>, BackendError> {
    let budget = chunk::input_budget(INSTRUCTION_TOKENS, PART_SUMMARY_TOKENS as usize);
    let chars = text.chars().count();
    let mut max_chars = chunk::target_chars(budget, chars, tokens);
    loop {
        let pieces = chunk::split(text, max_chars);
        let mut fits = true;
        for piece in &pieces {
            if piece.trim().is_empty() {
                continue;
            }
            if backend.count_tokens(piece).await? > budget {
                fits = false;
                break;
            }
        }
        if fits || max_chars <= 200 {
            return Ok(pieces
                .into_iter()
                .filter(|p| !p.trim().is_empty())
                .collect());
        }
        max_chars = max_chars * 3 / 4;
    }
}

fn part_request(piece: &str, n: usize, total: usize, focus: &str) -> ChatRequest {
    let mut system = format!(
        "{RULES} This is part {n} of {total} of a longer text. Summarise this part in up to \
         eight bullet points, keeping every concrete fact another reader would need."
    );
    push_focus(&mut system, focus);
    ChatRequest::new(vec![
        ChatMessage::system(system),
        ChatMessage::user(format!("Summarise this part:\n\n{piece}")),
    ])
    .with_max_tokens(PART_SUMMARY_TOKENS)
}

fn final_request(text: &str, focus: &str, length: SummaryLength, combining: bool) -> ChatRequest {
    let mut system = format!("{RULES} {}", length.instruction());
    push_focus(&mut system, focus);
    let user = if combining {
        format!(
            "The notes below summarise consecutive parts of one long text. \
             Write one summary of the whole text from them:\n\n{text}"
        )
    } else {
        format!("Summarise this text:\n\n{text}")
    };
    ChatRequest::new(vec![ChatMessage::system(system), ChatMessage::user(user)])
        .with_max_tokens(length.max_tokens())
}

fn push_focus(system: &mut String, focus: &str) {
    let focus = focus.trim();
    if !focus.is_empty() {
        system.push_str(&format!(
            " Focus on: {focus}. Leave out anything unrelated."
        ));
    }
}

async fn call(backend: &dyn Backend, request: ChatRequest) -> Result<String, BackendError> {
    let response = backend.chat(request).await?;
    match response.text().map(str::trim) {
        Some(text) if !text.is_empty() => Ok(text.to_owned()),
        _ => Err(BackendError::BadResponse(
            "the model returned an empty answer".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::backend::ChatResponse;

    /// Counts 4 characters per token and answers each call with a numbered summary.
    struct Scripted {
        calls: Mutex<Vec<String>>,
    }

    impl Backend for Scripted {
        fn chat(&self, request: ChatRequest) -> BoxFuture<'_, Result<ChatResponse, BackendError>> {
            let mut calls = self.calls.lock().unwrap();
            calls.push(request.messages[1].content.text().to_owned());
            let reply = format!("summary {}", calls.len());
            Box::pin(async move {
                Ok(serde_json::from_value(serde_json::json!({
                    "choices": [{"message": {"content": reply}}]
                }))
                .unwrap())
            })
        }

        fn count_tokens<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<usize, BackendError>> {
            Box::pin(async move { Ok(text.chars().count().div_ceil(4)) })
        }
    }

    fn scripted() -> Scripted {
        Scripted {
            calls: Mutex::new(Vec::new()),
        }
    }

    fn run(backend: &Scripted, text: &str) -> Result<Summary, SummariseError> {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(summarise(
                backend,
                text,
                "",
                SummaryLength::Medium,
                &NoProgress,
            ))
    }

    #[test]
    fn short_text_should_take_one_call() {
        let backend = scripted();
        let summary = run(&backend, "A short note.").ok().unwrap();
        assert_eq!((summary.parts, backend.calls.lock().unwrap().len()), (1, 1));
    }

    #[test]
    fn long_text_should_summarise_parts_then_combine() {
        let backend = scripted();
        // ~20K tokens at 4 chars per token: about 3 parts plus one combining call.
        let text = "The service restarted after the disk filled up. ".repeat(1700);
        let summary = run(&backend, &text).ok().unwrap();
        let calls = backend.calls.lock().unwrap();
        assert!(
            summary.parts >= 3
                && calls.len() == summary.parts + 1
                && calls.last().unwrap().contains("Part 1:"),
            "parts={} calls={}",
            summary.parts,
            calls.len()
        );
    }

    #[test]
    fn text_over_the_cap_should_be_refused_without_calling_the_model() {
        let backend = scripted();
        let text = "x".repeat((chunk::MAX_INPUT_TOKENS + 10) * 4);
        assert!(matches!(
            run(&backend, &text),
            Err(SummariseError::TooLong(Some(_)))
        ));
        assert!(backend.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn huge_text_should_be_refused_by_length_alone() {
        let backend = scripted();
        let text = "x".repeat(chunk::MAX_INPUT_TOKENS * chunk::MAX_CHARS_PER_TOKEN + 1);
        assert!(matches!(
            run(&backend, &text),
            Err(SummariseError::TooLong(None))
        ));
    }
}
