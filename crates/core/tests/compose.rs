//! Writing in chat: the compose panel, its keyboard and quick messages, sending and what Twitch
//! answers, the sign-in it needs, and the room's rules.

use htpc_core::*;

fn stream(channel: &str, user_id: &str) -> Tile {
    Tile::Stream(Stream {
        channel: channel.into(),
        user_id: user_id.into(),
        title: format!("{channel} title"),
        category: "Cat".into(),
        viewers: 1_000,
        tint: Rgb::default(),
        thumbnail: None,
    })
}

fn user(can_chat: bool) -> User {
    User {
        login: "viewer".into(),
        can_chat,
    }
}

/// Home with two streams, signed in (or not).
fn home(signed_in: Option<User>) -> AppState {
    let (mut s, _) = init();
    s.user = signed_in;
    let rows = vec![Row::new(
        RowKind::TopLive,
        vec![stream("chan", "42"), stream("other", "43")],
    )];
    reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    )
    .0
}

/// The first stream playing with chat shown.
fn watching(signed_in: Option<User>) -> AppState {
    let (s, _) = reduce(&home(signed_in), Intent::Select);
    let (mut s, _) = reduce(&s, Event::PlaybackStarted);
    s.chat_open = true;
    s
}

fn run(s: &AppState, intents: &[Intent]) -> (AppState, Vec<Command>) {
    let mut state = s.clone();
    let mut all = Vec::new();
    for intent in intents {
        let (next, cmds) = reduce(&state, *intent);
        state = next;
        all.extend(cmds);
    }
    (state, all)
}

/// Signed in with chat rights, the compose panel open.
fn composing() -> AppState {
    let (s, _) = reduce(&watching(Some(user(true))), Intent::Retry);
    assert!(s.composing());
    s
}

fn sends(cmds: &[Command]) -> Vec<(u32, String, String)> {
    cmds.iter()
        .filter_map(|c| match c {
            Command::SendChat {
                id,
                broadcaster_id,
                text,
                ..
            } => Some((*id, broadcaster_id.clone(), text.clone())),
            _ => None,
        })
        .collect()
}

/// Types `text` with a physical keyboard.
fn typed(s: &AppState, text: &str) -> AppState {
    let intents: Vec<Intent> = text.chars().map(Intent::Text).collect();
    run(s, &intents).0
}

#[test]
fn x_with_chat_shown_opens_compose_signed_in_only() {
    let signed_out = watching(None);
    let (s, cmds) = reduce(&signed_out, Intent::Retry);
    assert!(!s.compose.open, "signed out, chatting is off");
    assert!(cmds.is_empty());

    let (s, _) = reduce(&watching(Some(user(true))), Intent::Retry);
    assert!(s.compose.open);
    assert_eq!(s.compose.keys.board(), Board::Letters);

    // With chat hidden, X is not "write in chat".
    let mut hidden = watching(Some(user(true)));
    hidden.chat_open = false;
    let (s, _) = reduce(&hidden, Intent::Retry);
    assert!(!s.compose.open);
}

#[test]
fn the_player_menu_row_opens_compose_and_shows_chat() {
    let mut s = watching(Some(user(true)));
    s.chat_open = false;
    let (s, _) = reduce(&s, Intent::Menu);
    let s = (0..8).fold(s, |s, _| reduce(&s, Intent::Down).0);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::WriteChat));
    // Left and Right do nothing on an action row.
    let (same, _) = run(&s, &[Intent::Left, Intent::Right]);
    assert!(!same.compose.open);
    let (s, _) = reduce(&s, Intent::Select);
    assert!(s.compose.open && s.chat_open);
    assert_eq!(s.player_menu, None);

    // Signed out the row does nothing.
    let (s, _) = reduce(&watching(None), Intent::Menu);
    let s = (0..8).fold(s, |s, _| reduce(&s, Intent::Down).0);
    let (s, _) = reduce(&s, Intent::Select);
    assert!(!s.compose.open);
}

#[test]
fn the_keys_type_with_shift_once_then_locked() {
    let s = composing();
    // `a`, then shift (bottom left of row 3) and `a` again: a capital once, then lower case.
    let (s, _) = reduce(&s, Intent::Select);
    assert_eq!(s.compose.text, "a");
    let (s, _) = run(&s, &[Intent::Down, Intent::Down, Intent::Down]);
    assert_eq!(s.compose.keys.key(), Key::Shift);
    let (s, _) = reduce(&s, Intent::Select);
    assert_eq!(s.compose.shift, Shift::Once);
    let (s, _) = run(
        &s,
        &[
            Intent::Up,
            Intent::Up,
            Intent::Up,
            Intent::Select,
            Intent::Select,
        ],
    );
    assert_eq!(s.compose.text, "aAa");
    assert_eq!(s.compose.shift, Shift::Off);

    // Pressed twice it locks.
    let (s, _) = run(
        &s,
        &[
            Intent::Down,
            Intent::Down,
            Intent::Down,
            Intent::Select,
            Intent::Select,
        ],
    );
    assert_eq!(s.compose.shift, Shift::Lock);
    let (s, _) = run(
        &s,
        &[
            Intent::Up,
            Intent::Up,
            Intent::Up,
            Intent::Select,
            Intent::Select,
        ],
    );
    assert_eq!(s.compose.text, "aAaAA");
}

#[test]
fn the_shoulder_buttons_turn_the_pages() {
    let s = composing();
    let (s, _) = reduce(&s, Intent::PageRight);
    assert_eq!(s.compose.keys.board(), Board::Symbols);
    let (s, _) = reduce(&s, Intent::Select);
    assert_eq!(s.compose.text, "1");
    let (s, _) = reduce(&s, Intent::PageRight);
    assert_eq!(s.compose.keys.board(), Board::Emotes);
    // Emotes are words of their own.
    let (s, _) = run(&s, &[Intent::Right, Intent::Select]);
    assert_eq!(s.compose.text, "1 Kappa ");
    let (s, _) = reduce(&s, Intent::PageRight);
    assert_eq!(s.compose.keys.board(), Board::Letters);
    let (s, _) = reduce(&s, Intent::PageLeft);
    assert_eq!(s.compose.keys.board(), Board::Emotes);
}

#[test]
fn x_deletes_y_types_a_space_and_start_sends() {
    let s = typed(&composing(), "gg");
    let (s, _) = run(&s, &[Intent::ToggleChat, Intent::ToggleChat]);
    assert_eq!(s.compose.text, "gg ", "no double space");
    let s = typed(&s, "wpp");
    let (s, _) = reduce(&s, Intent::Retry);
    assert_eq!(s.compose.text, "gg wp");
    assert!(s.chat_open, "Y typed, it did not hide chat");

    let (s, cmds) = reduce(&s, Intent::Menu);
    assert_eq!(sends(&cmds), [(1, "42".to_owned(), "gg wp".to_owned())]);
    assert_eq!(s.compose.sending.as_ref().map(|m| m.id), Some(1));
    assert!(s.compose.open, "stays open for the next message");

    // One message at a time.
    let (s, cmds) = reduce(&s, Intent::Menu);
    assert!(sends(&cmds).is_empty());

    // Posted: the box clears.
    let (s, _) = reduce(&s, Event::ChatSent { id: 1 });
    assert_eq!(s.compose.text, "");
    assert_eq!(s.compose.sending, None);
    assert!(s.compose.open);
}

#[test]
fn an_empty_message_is_not_sent() {
    let s = typed(&composing(), " ");
    let (_, cmds) = reduce(&s, Intent::Menu);
    assert!(sends(&cmds).is_empty());
}

#[test]
fn a_dropped_message_keeps_its_text_and_says_why() {
    let s = typed(&composing(), "hello");
    let (s, _) = reduce(&s, Intent::Menu);
    let id = s.compose.sending.as_ref().unwrap().id;
    let problem = SendProblem::Dropped {
        code: "msg_followersonly".into(),
        message: "This room is in followers-only mode.".into(),
    };
    let (s, _) = reduce(
        &s,
        Event::ChatDropped {
            id,
            problem: problem.clone(),
        },
    );
    assert_eq!(s.compose.text, "hello");
    assert_eq!(s.compose.problem, Some(problem));
    assert_eq!(s.compose.sending, None);

    // Editing clears the reason; sending again tries again.
    let (edited, _) = reduce(&s, Intent::Text('!'));
    assert_eq!(edited.compose.problem, None);
    let (_, cmds) = reduce(&s, Intent::Menu);
    assert_eq!(sends(&cmds)[0].2, "hello");
}

#[test]
fn the_rate_limit_holds_a_message_back_and_keeps_it() {
    let s = typed(&composing(), "spam");
    let (s, _) = reduce(&s, Intent::Menu);
    let id = s.compose.sending.as_ref().unwrap().id;
    let (s, _) = reduce(
        &s,
        Event::ChatDropped {
            id,
            problem: SendProblem::TooFast,
        },
    );
    assert_eq!(s.compose.text, "spam");
    assert_eq!(s.compose.problem, Some(SendProblem::TooFast));
}

#[test]
fn late_answers_for_another_message_are_ignored() {
    let s = typed(&composing(), "one");
    let (s, _) = reduce(&s, Intent::Menu);
    let (after, _) = reduce(&s, Event::ChatSent { id: 99 });
    assert_eq!(after.compose.text, "one");
    assert!(after.compose.sending.is_some());
    let (after, _) = reduce(
        &s,
        Event::ChatDropped {
            id: 99,
            problem: SendProblem::TooFast,
        },
    );
    assert_eq!(after.compose.problem, None);
}

#[test]
fn quick_messages_send_at_once_and_leave_the_box_alone() {
    let s = typed(&composing(), "draft");
    // Up from the top row of keys reaches the quick messages.
    let (s, _) = reduce(&s, Intent::Up);
    assert_eq!(s.compose.chip, Some(0));
    let (s, _) = run(&s, &[Intent::Right, Intent::Right, Intent::Left]);
    assert_eq!(s.compose.chip, Some(1));
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(sends(&cmds), [(1, "42".to_owned(), "LUL".to_owned())]);
    let (s, _) = reduce(&s, Event::ChatSent { id: 1 });
    assert_eq!(
        s.compose.text, "draft",
        "the quick message did not clear the draft"
    );
    // Right stops at the last one, Down goes back to the keys.
    let (s, _) = run(&s, &[Intent::Right; 10]);
    assert_eq!(s.compose.chip, Some(4));
    let (s, _) = reduce(&s, Intent::Down);
    assert_eq!(s.compose.chip, None);
    assert_eq!(s.compose.keys.key(), Key::Char('a'));
}

#[test]
fn the_quick_messages_come_from_the_state() {
    let mut s = composing();
    s.quick_messages = vec!["hi chat".into()];
    let (s, _) = run(&s, &[Intent::Up, Intent::Right]);
    assert_eq!(s.compose.chip, Some(0));
    let (_, cmds) = reduce(&s, Intent::Select);
    assert_eq!(sends(&cmds)[0].2, "hi chat");

    // None at all: Up stays on the keys.
    let mut none = composing();
    none.quick_messages.clear();
    let (none, _) = reduce(&none, Intent::Up);
    assert_eq!(none.compose.chip, None);
}

#[test]
fn b_closes_compose_and_the_text_waits() {
    let s = typed(&composing(), "later");
    let (s, cmds) = reduce(&s, Intent::Back);
    assert!(!s.compose.open);
    assert_eq!(
        s.screen,
        Screen::Player,
        "B closed the panel, not the stream"
    );
    assert!(cmds.is_empty());
    assert_eq!(s.compose.text, "later");
    let (s, _) = reduce(&s, Intent::Retry);
    assert!(s.compose.open);
    assert_eq!(s.compose.text, "later");
}

#[test]
fn changing_channel_clears_compose_and_the_room() {
    let s = typed(&composing(), "for chan");
    let change = RoomModeChange {
        slow_secs: Some(30),
        ..RoomModeChange::default()
    };
    let (s, _) = reduce(
        &s,
        Event::ChatRoomMode {
            channel: "chan".into(),
            change,
        },
    );
    assert_eq!(s.room_mode.slow_secs, 30);
    // Another channel's rules are not ours.
    let (other, _) = reduce(
        &s,
        Event::ChatRoomMode {
            channel: "other".into(),
            change: RoomModeChange {
                subs_only: Some(true),
                ..RoomModeChange::default()
            },
        },
    );
    assert!(!other.room_mode.subs_only);

    let (s, _) = run(&s, &[Intent::Back, Intent::Back]);
    assert_eq!(s.screen, Screen::Home);
    assert_eq!(s.compose, Compose::default());
    assert_eq!(s.room_mode, RoomMode::default());

    // Following a raid moves to another channel too.
    let s = typed(&composing(), "for chan");
    let (s, _) = reduce(
        &s,
        Event::Raided {
            from_login: "chan".into(),
            to_login: "next".into(),
            to_name: "Next".into(),
            viewers: 5,
        },
    );
    assert!(s.compose.open, "the raid card is on top, compose waits");
    let (s, _) = reduce(&s, Intent::Select);
    assert_eq!(s.now_playing.as_ref().unwrap().channel, "next");
    assert_eq!(s.compose, Compose::default());
}

#[test]
fn a_physical_keyboard_types_into_compose() {
    let s = typed(&composing(), "yr Hi!");
    assert_eq!(
        s.compose.text, "yr Hi!",
        "y and r are letters here, case is kept"
    );
    assert!(s.chat_open);
    let (s, _) = reduce(&s, Intent::Erase);
    assert_eq!(s.compose.text, "yr Hi");
    // The 500-character limit.
    let long: String = "x".repeat(600);
    let s = typed(&s, &long);
    assert_eq!(s.compose.text.chars().count(), CHAT_MAX);

    // Not composing, `y` is still the chat shortcut.
    let (closed, _) = reduce(&s, Intent::Back);
    let (closed, _) = reduce(&closed, Intent::Text('y'));
    assert!(!closed.chat_open);
}

#[test]
fn a_token_without_chat_rights_asks_to_sign_in_again_and_comes_back() {
    let (s, _) = reduce(&watching(Some(user(false))), Intent::Retry);
    assert!(s.compose.open, "the panel opens on the sign-in message");
    // Typing goes nowhere.
    let s = typed(&s, "hi");
    assert_eq!(s.compose.text, "");
    let (none, cmds) = reduce(&s, Intent::Menu);
    assert!(sends(&cmds).is_empty());
    assert!(none.compose.open);

    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(s.screen, Screen::Login);
    assert!(cmds.contains(&Command::StartLogin));
    assert_eq!(s.after_login, Some(Screen::Player));

    let (s, _) = reduce(&s, Event::SignedIn(user(true)));
    assert_eq!(s.screen, Screen::Player, "back on the stream");
    assert!(s.composing(), "ready to write");
    assert!(s.can_chat());
    assert_eq!(s.now_playing.as_ref().unwrap().channel, "chan");
}

#[test]
fn b_on_the_sign_in_message_closes_it() {
    let (s, _) = reduce(&watching(Some(user(false))), Intent::Retry);
    let (s, cmds) = reduce(&s, Intent::Back);
    assert!(!s.compose.open);
    assert_eq!(s.screen, Screen::Player);
    assert!(cmds.is_empty());
}

#[test]
fn signing_out_ends_compose() {
    let s = typed(&composing(), "bye");
    let (s, _) = reduce(&s, Event::SignedOut);
    assert_eq!(s.compose, Compose::default());
    // And X no longer opens it.
    let (s, _) = reduce(&s, Intent::Retry);
    assert!(!s.compose.open);
}

#[test]
fn a_stream_without_a_known_user_id_still_sends() {
    // A followed channel that was offline has no user id: the app looks it up.
    let mut s = composing();
    s.now_playing.as_mut().unwrap().user_id.clear();
    let s = typed(&s, "hey");
    let (_, cmds) = reduce(&s, Intent::Menu);
    assert_eq!(sends(&cmds), [(1, String::new(), "hey".to_owned())]);
}

#[test]
fn the_raid_card_takes_the_buttons_while_composing() {
    let s = composing();
    let (s, _) = reduce(
        &s,
        Event::Raided {
            from_login: "chan".into(),
            to_login: "next".into(),
            to_name: "Next".into(),
            viewers: 5,
        },
    );
    // Typing does not reach the box behind the card, and B stays on this channel.
    let s = typed(&s, "x");
    assert_eq!(s.compose.text, "");
    let (s, _) = reduce(&s, Intent::Back);
    assert!(s.raid.is_none());
    assert!(s.composing());
    let s = typed(&s, "x");
    assert_eq!(s.compose.text, "x");
}
