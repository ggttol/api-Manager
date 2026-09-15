use std::borrow::Cow;

/// Encode the client's outer conventions envelope for Gemini's system prompt.
/// Google can reject this XML envelope with RESOURCE_EXHAUSTED even when the
/// same instructions with plain delimiters succeed. Do not rewrite mentions,
/// incomplete envelopes, or the instructions inside the envelope.
pub fn normalize_system_envelope<'a>(text: &'a str, model: &str) -> Cow<'a, str> {
    const OPEN: &str = "<system-conventions>";
    const CLOSE: &str = "</system-conventions>";
    if !model.starts_with("gemini") || !text.starts_with(OPEN) {
        return Cow::Borrowed(text);
    }
    let Some(close) = text.find("\n</system-conventions>") else {
        return Cow::Borrowed(text);
    };
    let close = close + 1;
    let end = close + CLOSE.len();
    if !text[end..].is_empty() && !text[end..].starts_with(['\r', '\n']) {
        return Cow::Borrowed(text);
    }
    let mut normalized = String::with_capacity(text.len());
    normalized.push_str("[system-conventions]");
    normalized.push_str(&text[OPEN.len()..close]);
    normalized.push_str("[/system-conventions]");
    normalized.push_str(&text[end..]);
    Cow::Owned(normalized)
}

/// Normalize an injected system-prompt segment and keep it isolated from the
/// surrounding markdown.
pub fn separated_prompt(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| format!("{text}\n\n"))
}

/// Detect only an already-injected full prompt. A substring match can suppress
/// a legitimate prompt when the user's own instructions merely quote it.
pub fn matches_prompt_segment(candidate: &str, prompt: &str) -> bool {
    let prompt = prompt.trim();
    !prompt.is_empty() && candidate.trim() == prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outer_envelope_changes_without_rewriting_instructions_or_examples() {
        let text = "<system-conventions>\n规则保持原样。\nExample: `<system-conventions>` and `</system-conventions>`\n</system-conventions>\n\nRemaining instructions.";
        let result = normalize_system_envelope(text, "gemini-3.8-flash-medium");
        assert_eq!(result, "[system-conventions]\n规则保持原样。\nExample: `<system-conventions>` and `</system-conventions>`\n[/system-conventions]\n\nRemaining instructions.");
        assert_eq!(
            normalize_system_envelope(&result, "gemini-3.8-flash-medium"),
            result
        );
    }

    #[test]
    fn non_envelopes_and_non_gemini_prompts_are_preserved() {
        for text in [
            "Explain <system-conventions>\nrules\n</system-conventions>",
            "<system-conventions>\nrules without a closing delimiter",
            "<system-conventions>\nrules\n</system-conventions> quoted suffix",
        ] {
            assert!(matches!(
                normalize_system_envelope(text, "gemini-3.8-flash-high"),
                Cow::Borrowed(_)
            ));
        }
        let text = "<system-conventions>\nrules\n</system-conventions>";
        assert_eq!(normalize_system_envelope(text, "claude-sonnet-4-6"), text);
    }

    #[test]
    fn prompt_segments_are_trimmed_separated_and_matched_exactly() {
        assert_eq!(
            separated_prompt("  # Global Rules\nDo this. \n"),
            Some("# Global Rules\nDo this.\n\n".to_string())
        );
        assert_eq!(separated_prompt(" \n\t"), None);
        assert!(matches_prompt_segment(
            "# Global Rules\nDo this.\n\n",
            " # Global Rules\nDo this. "
        ));
        assert!(!matches_prompt_segment(
            "User quoted: # Global Rules\nDo this.",
            "# Global Rules\nDo this."
        ));
    }
}
