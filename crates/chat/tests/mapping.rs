//! Chat server messages (captured-style IRC lines in `tests/fixtures/`) to core chat types.

use std::collections::HashMap;
use std::sync::Arc;

use htpc_chat::{BadgeBook, BadgeSet, EmoteProvider, Mapped, Mapper, TwitchCdn};
use htpc_core::{ChatKind, ChatMessage, ChatSpan, Moderation, Rgb};
use twitch_irc::message::{IRCMessage, ServerMessage};

fn fixture(name: &str) -> ServerMessage {
    let path = format!("{}/tests/fixtures/{name}.irc", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    // `<SOH>` stands for the control character that wraps /me messages.
    let raw = raw.trim_end_matches(['\r', '\n']).replace("<SOH>", "\u{1}");
    let irc = IRCMessage::parse(&raw).unwrap_or_else(|e| panic!("{name}: {e}"));
    ServerMessage::try_from(irc).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn badges() -> Arc<BadgeBook> {
    let book = BadgeBook::default();
    book.set_global(BadgeSet::from([
        (
            ("moderator".into(), "1".into()),
            "https://cdn/mod.png".into(),
        ),
        (
            ("subscriber".into(), "6".into()),
            "https://cdn/sub6.png".into(),
        ),
    ]));
    Arc::new(book)
}

fn mapper() -> Mapper {
    Mapper::new("chan", Arc::new(TwitchCdn), badges())
}

fn only_line(mapped: Vec<Mapped>) -> ChatMessage {
    match &mapped[..] {
        [Mapped::Line(line)] => line.clone(),
        other => panic!("expected one line, got {other:?}"),
    }
}

const KAPPA: &str = "https://static-cdn.jtvnw.net/emoticons/v2/25/static/dark/2.0";

#[test]
fn a_moderators_line_has_colour_badges_and_an_emote() {
    let line = only_line(mapper().map(fixture("privmsg_mod")));
    assert_eq!(line.id, "b34ccfc7-4977-403a-8a94-33c6bac34fb8");
    assert_eq!(
        (line.user.as_str(), line.login.as_str()),
        ("ModMarie", "modmarie")
    );
    assert_eq!(line.color, Some(Rgb::new(0x1E, 0x90, 0xFF)));
    assert_eq!(line.badges, ["https://cdn/mod.png", "https://cdn/sub6.png"]);
    assert_eq!(line.kind, ChatKind::Message);
    assert_eq!(
        line.spans,
        [
            ChatSpan::Text("nice".into()),
            ChatSpan::Emote {
                name: "Kappa".into(),
                url: KAPPA.into()
            },
            ChatSpan::Text("play".into()),
        ]
    );
    assert_eq!(line.text(), "nice Kappa play");
}

#[test]
fn a_me_message_is_an_action_without_the_marker() {
    let line = only_line(mapper().map(fixture("privmsg_action")));
    assert_eq!(line.kind, ChatKind::Action);
    assert_eq!(line.text(), "waves at chat");
    assert!(line.badges.is_empty());
}

#[test]
fn no_colour_and_odd_spacing_are_tidied() {
    let line = only_line(mapper().map(fixture("privmsg_plain_no_color")));
    assert_eq!(line.color, None);
    assert_eq!(line.user, "Plain_Pat");
    assert_eq!(line.spans, [ChatSpan::Text("hello there".into())]);
}

#[test]
fn unknown_badges_are_left_out() {
    // Only the moderator badge is known here.
    let book = BadgeBook::default();
    book.set_global(BadgeSet::from([(
        ("moderator".into(), "1".into()),
        "https://cdn/mod.png".into(),
    )]));
    let mapper = Mapper::new("chan", Arc::new(TwitchCdn), Arc::new(book));
    let line = only_line(mapper.map(fixture("privmsg_mod")));
    assert_eq!(line.badges, ["https://cdn/mod.png"]);
}

#[test]
fn another_channels_traffic_is_ignored() {
    assert!(mapper().map(fixture("privmsg_other_channel")).is_empty());
    // The match is not case sensitive.
    let upper = Mapper::new("CHAN", Arc::new(TwitchCdn), badges());
    assert_eq!(upper.map(fixture("privmsg_mod")).len(), 1);
}

#[test]
fn twitchs_own_announcements_become_notices() {
    let line = only_line(mapper().map(fixture("usernotice_raid")));
    assert_eq!(line.kind, ChatKind::Notice);
    assert_eq!(line.text(), "42 raiders from Raider have joined!");
    assert!(line.id.is_empty() && line.login.is_empty());
}

#[test]
fn an_announcement_with_text_but_no_system_message_is_a_normal_line() {
    let line = only_line(mapper().map(fixture("usernotice_announcement")));
    assert_eq!(line.kind, ChatKind::Message);
    assert_eq!(line.user, "ModMarie");
    assert_eq!(line.text(), "Stream Kappa starts soon");
    assert!(matches!(line.spans[1], ChatSpan::Emote { .. }));
}

#[test]
fn room_notices_are_shown_and_login_notices_are_not() {
    let line = only_line(mapper().map(fixture("notice_slow")));
    assert_eq!(line.kind, ChatKind::Notice);
    assert!(line.text().starts_with("This room is now in slow mode"));
    assert!(mapper().map(fixture("notice_prelogin")).is_empty());
}

#[test]
fn moderation_messages_name_who_or_what_to_remove() {
    let one = |name| match &mapper().map(fixture(name))[..] {
        [Mapped::Moderation(m)] => m.clone(),
        other => panic!("{name}: {other:?}"),
    };
    assert_eq!(
        one("clearchat_timeout"),
        Moderation::ClearUser("trouble_tim".into())
    );
    assert_eq!(one("clearchat_all"), Moderation::ClearAll);
    assert_eq!(
        one("clearmsg"),
        Moderation::Delete("3f1a2b4c-0000-4000-8000-aaaaaaaaaaaa".into())
    );
}

#[test]
fn traffic_that_is_not_chat_maps_to_nothing() {
    assert!(mapper().map(fixture("ping")).is_empty());
}

/// A stand-in for a 7TV-style set: words that are emotes, found by name.
struct ThirdParty(HashMap<&'static str, &'static str>);

impl EmoteProvider for ThirdParty {
    fn tagged(&self, id: &str) -> Option<String> {
        TwitchCdn.tagged(id)
    }

    fn word(&self, word: &str) -> Option<String> {
        self.0.get(word).map(|u| (*u).to_owned())
    }
}

#[test]
fn a_second_provider_adds_word_emotes_next_to_twitchs() {
    let provider = ThirdParty(HashMap::from([("OMEGALUL", "https://7tv/omega.webp")]));
    let mapper = Mapper::new("chan", Arc::new(provider), badges());
    let msg = fixture("privmsg_mod");
    let ServerMessage::Privmsg(mut privmsg) = msg else {
        panic!("a PRIVMSG fixture");
    };
    privmsg.message_text = "nice Kappa OMEGALUL so good".into();
    let line = only_line(mapper.map(ServerMessage::Privmsg(privmsg)));
    assert_eq!(
        line.spans,
        [
            ChatSpan::Text("nice".into()),
            ChatSpan::Emote {
                name: "Kappa".into(),
                url: KAPPA.into()
            },
            ChatSpan::Emote {
                name: "OMEGALUL".into(),
                url: "https://7tv/omega.webp".into()
            },
            ChatSpan::Text("so good".into()),
        ]
    );
    assert_eq!(line.image_urls().count(), 4, "two badges and two emotes");
}

#[test]
fn bad_emote_ranges_never_panic() {
    use twitch_irc::message::Emote;
    let emote = |id: &str, range: std::ops::Range<usize>, code: &str| Emote {
        id: id.into(),
        char_range: range,
        code: code.into(),
    };
    let build = |text: &str, emotes: &[Emote]| htpc_chat::build_spans(text, emotes, &TwitchCdn);

    // Past the end of the text: clipped.
    let spans = build("hi Kappa", &[emote("25", 3..40, "Kappa")]);
    assert_eq!(spans.len(), 2);
    // Starting past the end: dropped, the text stays.
    assert_eq!(
        build("hi", &[emote("25", 9..12, "Kappa")]),
        [ChatSpan::Text("hi".into())]
    );
    // Overlapping ranges: the second is skipped.
    let spans = build(
        "abcdef",
        &[emote("1", 0..4, "abcd"), emote("2", 2..6, "cdef")],
    );
    assert_eq!(spans.len(), 2);
    // An emote id that cannot be a URL stays text.
    let spans = build("x ../y", &[emote("../y", 2..6, "../y")]);
    assert_eq!(spans, [ChatSpan::Text("x ../y".into())]);
    // Multi-byte text is cut by characters, not bytes.
    let spans = build("héllo Kappa", &[emote("25", 6..11, "Kappa")]);
    assert_eq!(spans[0], ChatSpan::Text("héllo".into()));
}
