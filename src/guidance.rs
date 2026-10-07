//! Delegation guidance shipped inside the binary, so `fm-mcp install` can
//! write it without a network or a clone: a Claude Code skill and an
//! AGENTS.md block for Codex.

/// `skills/fm-delegate/SKILL.md`, installed as `~/.claude/skills/fm-delegate/SKILL.md`.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "written by `fm-mcp install` (Phase 5)")
)]
pub const SKILL: &str = include_str!("../skills/fm-delegate/SKILL.md");

/// The block inserted into Codex's global AGENTS.md, between the markers below.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "written by `fm-mcp install` (Phase 5)")
)]
pub const AGENTS_SNIPPET: &str = include_str!("../snippets/AGENTS.md.snippet");

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by `fm-mcp install` (Phase 5)")
)]
pub const BEGIN_MARKER: &str = "<!-- fm-mcp:begin";
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by `fm-mcp install` (Phase 5)")
)]
pub const END_MARKER: &str = "<!-- fm-mcp:end -->";

#[cfg(test)]
mod tests {
    use super::*;

    fn frontmatter_field<'a>(field: &str) -> &'a str {
        SKILL
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{field}: ")))
            .unwrap_or_default()
    }

    #[test]
    fn skill_name_should_be_fm_delegate() {
        assert_eq!(frontmatter_field("name"), "fm-delegate");
    }

    #[test]
    fn skill_description_should_say_when_to_use_it() {
        let description = frontmatter_field("description");
        assert!(
            description.starts_with("Use when") && description.len() < 500,
            "{description}"
        );
    }

    #[test]
    fn skill_should_stay_short() {
        assert!(SKILL.split_whitespace().count() < 500);
    }

    #[test]
    fn snippet_should_be_wrapped_in_markers() {
        let trimmed = AGENTS_SNIPPET.trim();
        assert!(trimmed.starts_with(BEGIN_MARKER) && trimmed.ends_with(END_MARKER));
    }

    /// Words the guidance tells agents to look for in tool errors. Each must
    /// appear in a real error message in `server.rs`, so they stay in step.
    const ERROR_PHRASES: &[&str] = &[
        "safety filter refused",
        "too long",
        "took longer than",
        "got stuck",
        "didn't match the schema",
        "not available",
        "could not start",
        "stopped responding",
        "keeps crashing",
        "could not measure",
        "a bug in fm-mcp",
    ];

    #[test]
    fn error_phrases_should_appear_in_real_tool_errors() {
        let server = include_str!("server.rs");
        for phrase in ERROR_PHRASES {
            assert!(server.contains(phrase), "no tool error contains {phrase:?}");
        }
    }

    #[test]
    fn skill_and_snippet_should_cover_every_error_phrase() {
        for phrase in ERROR_PHRASES {
            assert!(
                SKILL.contains(phrase) && AGENTS_SNIPPET.contains(phrase),
                "{phrase:?} missing from the skill or the snippet"
            );
        }
    }

    #[test]
    fn skill_and_snippet_should_give_the_same_log_size_limit() {
        let limit = "about 1,000 lines of a dense log";
        assert!(SKILL.contains(limit) && AGENTS_SNIPPET.contains(limit));
    }

    /// The `extract`/`classify` limit, measured 2026-10-07: 3,640 words of prose
    /// is 5,840 tokens, under the 6,518-token `extract` budget; 5,005 words is
    /// 8,030 tokens, over both budgets. The tool descriptions, the skill and the
    /// snippet must all give the same measured figure.
    #[test]
    fn guidance_and_descriptions_should_give_the_measured_prose_limit() {
        let limit = "about 3,500 words";
        let server = include_str!("server.rs");
        assert!(SKILL.contains(limit) && AGENTS_SNIPPET.contains(limit) && server.contains(limit));
        assert!(!SKILL.contains("5,000 words") && !AGENTS_SNIPPET.contains("5,000 words"));
    }
}
