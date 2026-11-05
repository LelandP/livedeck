//! Chat in the reducer: joining, the connection status, moderation and image requests.

use htpc_core::*;

fn live_stream(channel: &str) -> Tile {
    Tile::Stream(Stream {
        channel: channel.into(),
        title: "t".into(),
        category: "c".into(),
        viewers: 1,
        tint: Rgb::default(),
        thumbnail: None,
    })
}

/// Home with one stream, then opened on the Player.
fn watching() -> AppState {
    let (s, _) = init();
    let rows = vec![Row::new(RowKind::TopLive, vec![live_stream("chan")])];
    let (s, _) = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    );
    reduce(&s, Intent::Select).0
}

fn lines(channel: &str, messages: Vec<ChatMessage>) -> Event {
    Event::ChatMessages {
        channel: channel.into(),
        messages,
    }
}

fn emote_line(id: &str, url: &str) -> ChatMessage {
    ChatMessage {
        id: id.into(),
        badges: vec!["https://cdn/badge.png".into()],
        spans: vec![
            ChatSpan::Text("hi".into()),
            ChatSpan::Emote {
                name: "Kappa".into(),
                url: url.into(),
            },
        ],
        ..ChatMessage::plain("a", "")
    }
}

#[test]
fn opening_a_stream_starts_connecting() {
    let s = watching();
    assert_eq!(s.chat_status, ChatStatus::Connecting);
    assert!(s.chat.is_empty());
}

#[test]
fn leaving_resets_chat_and_status() {
    let (s, _) = reduce(
        &watching(),
        lines("chan", vec![ChatMessage::plain("a", "x")]),
    );
    let (s, cmds) = reduce(&s, Intent::Back);
    assert_eq!(s.chat_status, ChatStatus::Idle);
    assert!(s.chat.is_empty());
    assert!(cmds.contains(&Command::LeaveChat));
}

#[test]
fn status_events_apply_only_to_the_watched_channel() {
    let status = |channel: &str, status| Event::ChatStatus {
        channel: channel.into(),
        status,
    };
    let (s, _) = reduce(&watching(), status("chan", ChatStatus::Reconnecting));
    assert_eq!(s.chat_status, ChatStatus::Reconnecting);
    let (s, _) = reduce(&s, status("other", ChatStatus::Live));
    assert_eq!(
        s.chat_status,
        ChatStatus::Reconnecting,
        "other channel ignored"
    );

    // A late status after leaving must not revive the indicator.
    let (s, _) = reduce(&s, Intent::Back);
    let (s, _) = reduce(&s, status("chan", ChatStatus::Live));
    assert_eq!(s.chat_status, ChatStatus::Idle);
}

#[test]
fn a_line_proves_the_connection_is_live() {
    let (s, _) = reduce(
        &watching(),
        lines("chan", vec![ChatMessage::plain("a", "x")]),
    );
    assert_eq!(s.chat_status, ChatStatus::Live);
}

#[test]
fn toggling_chat_keeps_collecting_lines() {
    let (s, _) = reduce(&watching(), Intent::ToggleChat);
    assert!(s.chat_open);
    let (s, _) = reduce(&s, Intent::ToggleChat);
    assert!(!s.chat_open);
    let (s, cmds) = reduce(
        &s,
        lines("chan", vec![ChatMessage::plain("a", "while hidden")]),
    );
    assert_eq!(s.chat.len(), 1, "lines keep arriving while chat is hidden");
    assert!(cmds.is_empty(), "no reconnect or rejoin on a toggle");
}

#[test]
fn toggling_chat_never_emits_commands() {
    let (_, cmds) = reduce(&watching(), Intent::ToggleChat);
    assert!(cmds.is_empty());
}

#[test]
fn moderation_removes_lines_of_the_watched_channel_only() {
    let mut a = ChatMessage::plain("Spammer", "buy followers");
    a.id = "1".into();
    let mut b = ChatMessage::plain("friend", "hello");
    b.id = "2".into();
    let (s, _) = reduce(&watching(), lines("chan", vec![a, b]));

    let moderate = |channel: &str, action| Event::ChatModerated {
        channel: channel.into(),
        action,
    };
    let (kept, _) = reduce(&s, moderate("other", Moderation::ClearAll));
    assert_eq!(kept.chat.len(), 2);

    let (s2, _) = reduce(
        &s,
        moderate("chan", Moderation::ClearUser("spammer".into())),
    );
    assert_eq!(s2.chat.len(), 1);
    assert_eq!(s2.chat[0].user, "friend");

    let (s3, _) = reduce(&s, moderate("chan", Moderation::Delete("2".into())));
    assert_eq!(s3.chat[0].user, "Spammer");

    let (s4, _) = reduce(&s, moderate("chan", Moderation::ClearAll));
    assert!(s4.chat.is_empty());
}

#[test]
fn new_lines_request_each_image_once() {
    let url = "https://cdn/kappa.png";
    let batch = vec![emote_line("1", url), emote_line("2", url)];
    let (_, cmds) = reduce(&watching(), lines("chan", batch));
    let fetches: Vec<_> = cmds
        .iter()
        .filter_map(|c| match c {
            Command::FetchChatImages { urls } => Some(urls.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(fetches.len(), 1);
    // Badge first, then the emote, and the emote only once.
    assert_eq!(fetches[0], ["https://cdn/badge.png", url]);
}

#[test]
fn plain_lines_request_no_images() {
    let (_, cmds) = reduce(
        &watching(),
        lines("chan", vec![ChatMessage::plain("a", "x")]),
    );
    assert!(cmds.is_empty());
}

#[test]
fn late_lines_for_a_left_channel_request_nothing() {
    let (s, _) = reduce(&watching(), Intent::Back);
    let (s, cmds) = reduce(
        &s,
        lines("chan", vec![emote_line("1", "https://cdn/k.png")]),
    );
    assert!(s.chat.is_empty());
    assert!(cmds.is_empty());
}

#[test]
fn the_buffer_keeps_the_newest_200_lines() {
    let many = (0..CHAT_CAPACITY + 25)
        .map(|i| ChatMessage::plain(&format!("u{i}"), "x"))
        .collect();
    let (s, _) = reduce(&watching(), lines("chan", many));
    assert_eq!(s.chat.len(), CHAT_CAPACITY);
    assert_eq!(s.chat[0].user, "u25");
}
