//! Token budget and chunking for the model's ~8K-token context.
//!
//! Measured facts behind the numbers are in AGENTS.md: the context is about
//! 8K tokens, the chat framing adds about 60, and characters per token range
//! from 1.6 (logs) to 4.2 (unusual words), so sizes are always checked with
//! real token counts rather than a fixed ratio.

/// The model's context: input, framing and the answer must all fit.
pub const CONTEXT_TOKENS: usize = 8192;
/// Tokens the chat format adds around the messages (measured ~55–60).
pub const FRAMING_TOKENS: usize = 64;
/// About 5% of the context, kept free for counting differences.
pub const SAFETY_MARGIN_TOKENS: usize = 410;
/// Largest total input `summarise` accepts (about 8 chunks).
pub const MAX_INPUT_TOKENS: usize = 48_000;
/// No text type measured has more characters per token than this (random
/// dictionary words: 4.2), so more than `MAX_INPUT_TOKENS * 5` characters is
/// certainly over the cap and can be refused without counting.
pub const MAX_CHARS_PER_TOKEN: usize = 5;

/// Input tokens available in one call, given the instruction size and the
/// tokens reserved for the answer.
pub fn input_budget(instruction_tokens: usize, max_output_tokens: usize) -> usize {
    CONTEXT_TOKENS.saturating_sub(
        instruction_tokens + FRAMING_TOKENS + max_output_tokens + SAFETY_MARGIN_TOKENS,
    )
}

/// True when `text` is certainly over `MAX_INPUT_TOKENS`, judged by length alone.
pub fn certainly_over_cap(text: &str) -> bool {
    text.chars().count() > MAX_INPUT_TOKENS * MAX_CHARS_PER_TOKEN
}

/// Characters to aim for per chunk, so that a chunk of this text lands under
/// `budget` tokens. `chars` and `tokens` describe the whole text, which gives
/// this text's own characters-per-token rate; 90% leaves room for unevenness.
pub fn target_chars(budget: usize, chars: usize, tokens: usize) -> usize {
    let per_token = chars as f64 / tokens.max(1) as f64;
    ((budget as f64 * per_token * 0.9) as usize).max(1)
}

/// Splits `text` into chunks of at most `max_chars` characters. Breaks at
/// paragraph boundaries where possible, then lines, then sentences, and only
/// cuts inside a sentence when one alone is too long. Joining the chunks gives
/// back the original text.
pub fn split(text: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(1);
    let mut units = Vec::new();
    split_units(text, max_chars, 0, &mut units);

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;
    for unit in units {
        let len = unit.chars().count();
        if current_len + len > max_chars && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            current_len = 0;
        }
        current.push_str(&unit);
        current_len += len;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

const SEPARATORS: [&str; 3] = ["\n\n", "\n", ". "];

/// Breaks `text` into pieces of at most `max_chars`, using coarser separators first.
fn split_units(text: &str, max_chars: usize, level: usize, out: &mut Vec<String>) {
    if text.chars().count() <= max_chars {
        out.push(text.to_owned());
        return;
    }
    match SEPARATORS.get(level) {
        Some(separator) => {
            for part in text.split_inclusive(separator) {
                split_units(part, max_chars, level + 1, out);
            }
        }
        None => {
            let chars: Vec<char> = text.chars().collect();
            for piece in chars.chunks(max_chars) {
                out.push(piece.iter().collect());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_budget_should_leave_room_for_framing_answer_and_margin() {
        assert_eq!(input_budget(100, 400), 8192 - 100 - 64 - 400 - 410);
    }

    #[test]
    fn input_budget_should_not_underflow() {
        assert_eq!(input_budget(9000, 400), 0);
    }

    #[test]
    fn split_should_return_short_text_unchanged() {
        assert_eq!(split("one. two.", 100), vec!["one. two."]);
    }

    #[test]
    fn split_should_prefer_paragraph_boundaries() {
        let text = "aaaa aaaa.\n\nbbbb bbbb.\n\ncccc cccc.";
        assert_eq!(
            split(text, 24),
            vec!["aaaa aaaa.\n\nbbbb bbbb.\n\n", "cccc cccc."]
        );
    }

    #[test]
    fn split_should_fall_back_to_sentences() {
        let text = "First sentence here. Second sentence here. Third one.";
        let chunks = split(text, 25);
        assert_eq!(
            chunks,
            vec![
                "First sentence here. ",
                "Second sentence here. ",
                "Third one."
            ]
        );
    }

    #[test]
    fn split_should_cut_a_too_long_sentence_at_char_boundaries() {
        let chunks = split(&"é".repeat(25), 10);
        assert_eq!(
            chunks.iter().map(|c| c.chars().count()).collect::<Vec<_>>(),
            vec![10, 10, 5]
        );
    }

    #[test]
    fn split_should_never_exceed_max_chars() {
        let text =
            "Line one is here.\nLine two is longer than the others.\n\nPara two. ".repeat(40);
        assert!(split(&text, 37).iter().all(|c| c.chars().count() <= 37));
    }

    #[test]
    fn split_should_keep_all_text() {
        let text = "Alpha beta.\n\nGamma delta. Epsilon zeta.\nEta theta iota kappa.".repeat(7);
        assert_eq!(split(&text, 30).concat(), text);
    }

    #[test]
    fn target_chars_should_use_the_texts_own_density() {
        // Dense logs: 1.6 chars per token, so a 1000-token budget is ~1440 chars.
        assert_eq!(target_chars(1000, 16_000, 10_000), 1440);
    }

    #[test]
    fn certainly_over_cap_should_only_trigger_beyond_five_chars_per_token() {
        let at_limit = "x".repeat(MAX_INPUT_TOKENS * MAX_CHARS_PER_TOKEN);
        assert!(!certainly_over_cap(&at_limit));
        assert!(certainly_over_cap(&format!("{at_limit}x")));
    }
}
