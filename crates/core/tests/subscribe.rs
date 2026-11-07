//! The subscribe card (player menu): a QR code of the channel's subscribe page, and outside Game
//! Mode a button that opens it in a browser.

use htpc_core::*;

fn home() -> AppState {
    let (s, _) = init();
    let tile = Tile::Stream(Stream {
        channel: ChannelRef::twitch("chan_1"),
        user_id: String::new(),
        title: "title".into(),
        category: "Cat".into(),
        viewers: 1_000,
        tint: Rgb::default(),
        thumbnail: None,
    });
    reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(RowKind::TopLive, vec![tile])],
        },
    )
    .0
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

/// Playing, with the menu open on the Subscribe row.
fn on_subscribe_row(can_open_browser: bool) -> AppState {
    let mut s = home();
    s.can_open_browser = can_open_browser;
    let (s, _) = reduce(&s, Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted).0;
    let (s, _) = run(&s, &[Intent::Menu]);
    let (s, _) = run(&s, &[Intent::Down; 7]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::Subscribe));
    s
}

fn card_open(can_open_browser: bool) -> AppState {
    let (s, cmds) = reduce(&on_subscribe_row(can_open_browser), Intent::Select);
    assert!(s.subscribe_card);
    assert!(cmds.is_empty(), "{cmds:?}");
    s
}

#[test]
fn the_url_is_the_channels_subscribe_page() {
    assert_eq!(subscribe_url("chan_1"), "https://www.twitch.tv/subs/chan_1");
}

#[test]
fn a_on_the_row_opens_the_card_and_the_stream_keeps_playing() {
    let s = card_open(false);
    assert_eq!(s.screen, Screen::Player);
    assert_eq!(s.playback, Playback::Playing);
    assert!(!s.paused);
}

#[test]
fn left_and_right_skip_the_action_row() {
    let s = on_subscribe_row(true);
    let (s, cmds) = run(&s, &[Intent::Left, Intent::Right]);
    assert!(!s.subscribe_card);
    assert!(cmds.is_empty(), "{cmds:?}");
    assert_eq!(s.player_menu, Some(PlayerMenuRow::Subscribe));
}

#[test]
fn b_closes_the_card_back_to_the_menu() {
    let (s, cmds) = reduce(&card_open(true), Intent::Back);
    assert!(!s.subscribe_card);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::Subscribe));
    assert_eq!(s.screen, Screen::Player);
    assert!(cmds.is_empty(), "{cmds:?}");
}

#[test]
fn a_opens_the_browser_where_there_is_one() {
    let (s, cmds) = reduce(&card_open(true), Intent::Select);
    assert_eq!(
        cmds,
        vec![Command::OpenUrl("https://www.twitch.tv/subs/chan_1".into())]
    );
    assert!(s.subscribe_card, "the card stays open");
}

#[test]
fn a_does_nothing_in_game_mode() {
    let (s, cmds) = reduce(&card_open(false), Intent::Select);
    assert!(cmds.is_empty(), "{cmds:?}");
    assert!(s.subscribe_card);
}

#[test]
fn other_buttons_are_ignored_while_the_card_is_open() {
    let open = card_open(true);
    let (s, cmds) = run(
        &open,
        &[
            Intent::Up,
            Intent::Down,
            Intent::Left,
            Intent::Right,
            Intent::ToggleChat,
            Intent::Retry,
        ],
    );
    assert!(cmds.is_empty(), "{cmds:?}");
    assert!(s.subscribe_card);
    assert_eq!(s.player_menu, open.player_menu);
    assert_eq!(s.chat_open, open.chat_open);
    assert_eq!(s.session, open.session);
}

#[test]
fn leaving_the_player_closes_the_card() {
    // The sleep timer can leave the player while the card is open.
    let mut s = card_open(true);
    s.sleep = Some(SleepTimer {
        id: 9,
        minutes: 15,
        left_min: 0,
        warning: false,
    });
    let (s, _) = reduce(&s, Event::SleepTimerFired { id: 9 });
    assert_eq!(s.screen, Screen::Home);
    assert!(!s.subscribe_card);
}

#[test]
fn works_signed_out() {
    let s = card_open(true);
    assert!(!s.is_signed_in());
    let (_, cmds) = reduce(&s, Intent::Select);
    assert_eq!(cmds.len(), 1);
}
