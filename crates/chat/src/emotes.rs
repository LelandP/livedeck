//! Where emote images come from.
//!
//! Twitch tags the emotes it knows by id, and its CDN serves them. Third-party emote sets
//! (7TV, BTTV, FFZ) are plain words in the message, so they are looked up by name. Both go through
//! [`EmoteProvider`], so adding a set later means a new provider and no change to the mapping.

/// Resolves emote images for chat lines.
pub trait EmoteProvider: Send + Sync + 'static {
    /// The image of an emote Twitch itself tagged in the message, by id.
    fn tagged(&self, id: &str) -> Option<String>;

    /// The image of a word that may be a third-party emote (a channel's 7TV set, say). Twitch's
    /// own emotes are tagged, so the default knows no words.
    fn word(&self, _word: &str) -> Option<String> {
        None
    }
}

/// Twitch's own CDN: the emotes the chat server tags by id.
#[derive(Debug, Clone, Copy, Default)]
pub struct TwitchCdn;

/// Static (first frame) images, so animated emotes decode as PNG. 2.0 is 56 px: sharp at the
/// 28 px a chat line uses, also at a 2x display scale.
const CDN: &str = "https://static-cdn.jtvnw.net/emoticons/v2";

impl EmoteProvider for TwitchCdn {
    fn tagged(&self, id: &str) -> Option<String> {
        // Ids are alphanumeric with `_` (`301512758_TK`, `emotesv2_ab12`). Anything else came
        // from a broken or hostile tag and must not be spliced into a URL.
        let ok = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        ok.then(|| format!("{CDN}/{id}/static/dark/2.0"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagged_emotes_use_the_static_cdn_image() {
        assert_eq!(
            TwitchCdn.tagged("25").as_deref(),
            Some("https://static-cdn.jtvnw.net/emoticons/v2/25/static/dark/2.0")
        );
        assert!(TwitchCdn.tagged("301512758_TK").is_some());
        assert!(TwitchCdn.tagged("emotesv2_a1b2").is_some());
    }

    #[test]
    fn odd_ids_are_refused() {
        for id in ["", "../x", "25/../../evil", "a b", "a?b=c", "é"] {
            assert_eq!(TwitchCdn.tagged(id), None, "{id:?}");
        }
    }

    #[test]
    fn twitch_knows_no_plain_word_emotes() {
        assert_eq!(TwitchCdn.word("Kappa"), None);
    }
}
