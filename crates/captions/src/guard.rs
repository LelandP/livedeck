/// Phrases Whisper invents out of silence, noise and music.
const SILENCE_PHRASES: &[&str] = &[
    "thank you",
    "thanks for watching",
    "thank you for watching",
    "thanks for watching and see you next time",
    "please subscribe",
    "bye",
    "you",
];

/// True when `text` should not be shown.
///
/// Empty text, text with no letters or digits, and sound tags such as `[BLANK_AUDIO]` or `(music)`
/// always go. A known silence phrase goes when the segment was short (under 3 s) or quiet
/// (`rms` under 0.01), because that is when Whisper makes them up.
pub fn is_hallucination(text: &str, duration_s: f64, rms: f32) -> bool {
    let t = text.trim();
    if t.is_empty() || !t.chars().any(char::is_alphanumeric) {
        return true;
    }
    if is_sound_tag(t) {
        return true;
    }
    let plain: String = t
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .to_lowercase();
    let plain = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    SILENCE_PHRASES.contains(&plain.as_str()) && (duration_s < 3.0 || rms < 0.01)
}

fn is_sound_tag(t: &str) -> bool {
    (t.starts_with('[') && t.ends_with(']'))
        || (t.starts_with('(') && t.ends_with(')'))
        || (t.starts_with('*') && t.ends_with('*') && t.len() > 1)
        || (t.starts_with('♪'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_punctuation_only_go() {
        assert!(is_hallucination("", 5.0, 0.1));
        assert!(is_hallucination("   \n", 5.0, 0.1));
        assert!(is_hallucination(" ... ", 5.0, 0.1));
    }

    #[test]
    fn sound_tags_go() {
        assert!(is_hallucination("[BLANK_AUDIO]", 5.0, 0.1));
        assert!(is_hallucination("(music)", 5.0, 0.1));
        assert!(is_hallucination("[ Silence ]", 5.0, 0.1));
        assert!(is_hallucination("♪♪ la la ♪♪", 5.0, 0.1));
    }

    #[test]
    fn silence_phrases_go_when_short_or_quiet() {
        assert!(is_hallucination("Thank you.", 1.2, 0.1));
        assert!(is_hallucination(" Thanks for watching! ", 2.0, 0.1));
        assert!(is_hallucination("Thank you.", 6.0, 0.002));
    }

    #[test]
    fn silence_phrases_stay_when_long_and_loud() {
        assert!(!is_hallucination("Thank you.", 6.0, 0.1));
    }

    #[test]
    fn real_text_stays() {
        assert!(!is_hallucination(
            "Ask not what your country can do.",
            1.0,
            0.001
        ));
        assert!(!is_hallucination(
            "Thank you all for coming tonight.",
            1.0,
            0.1
        ));
    }
}
