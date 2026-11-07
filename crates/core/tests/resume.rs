//! Saving the multiview set and Home's *Resume multiview* card (plan.md, phase 7): the set is
//! saved whenever it changes, the card sits at the start of Home's first row of cards while at
//! least two of the set are live, and A on it starts the set again without the offline ones.

use htpc_core::*;

fn live(login: &str) -> Stream {
    Stream {
        channel: ChannelRef::twitch(login),
        user_id: format!("id_{login}"),
        title: format!("{login} title"),
        category: "Cat".into(),
        viewers: 1_000,
        tint: Rgb::default(),
        thumbnail: Some(format!("https://img/{login}.jpg")),
    }
}

fn offline(login: &str) -> Tile {
    Tile::Channel(Channel {
        channel: ChannelRef::twitch(login),
        name: login.into(),
        tint: Rgb::default(),
    })
}

fn run(s: &AppState, msgs: Vec<Msg>) -> (AppState, Vec<Command>) {
    let mut state = s.clone();
    let mut all = Vec::new();
    for msg in msgs {
        let (next, cmds) = reduce(&state, msg);
        state = next;
        all.extend(cmds);
    }
    (state, all)
}

fn press(s: &AppState, intents: &[Intent]) -> (AppState, Vec<Command>) {
    run(s, intents.iter().map(|i| Msg::from(*i)).collect())
}

fn user() -> User {
    User {
        login: "viewer".into(),
        can_chat: true,
    }
}

/// The saved set: `a` ... `e` in a grid of 6, `c` with sound.
fn saved() -> SavedSet {
    SavedSet {
        channels: ["a", "b", "c", "d", "e"]
            .into_iter()
            .map(ChannelRef::twitch)
            .collect(),
        layout: Layout::Grid(6),
        sound: 2,
    }
}

/// The provider's check of the saved set: `a`, `c` and `e` live, `b` and `d` offline.
fn checked() -> Row {
    Row::new(
        RowKind::Resume,
        vec![
            Tile::Stream(live("a")),
            offline("b"),
            Tile::Stream(live("c")),
            offline("d"),
            Tile::Stream(live("e")),
        ],
    )
}

fn top_live() -> Row {
    Row::new(
        RowKind::TopLive,
        (0..5)
            .map(|i| Tile::Stream(live(&format!("t{i}"))))
            .collect(),
    )
}

fn hero() -> Row {
    Row::new(RowKind::Featured, vec![Tile::Stream(live("hero"))])
}

/// Signed in, with the saved set, and Home loaded with `rows`.
fn home_with(rows: Vec<Row>) -> (AppState, Vec<Command>) {
    let (mut s, _) = init();
    s.user = Some(user());
    s.last_multiview = Some(saved());
    reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    )
}

fn home() -> AppState {
    home_with(vec![hero(), top_live(), checked()]).0
}

fn card_logins(tile: Option<&Tile>) -> Vec<String> {
    match tile {
        Some(Tile::Resume(card)) => card
            .streams
            .iter()
            .map(|s| s.channel.login.clone())
            .collect(),
        other => panic!("not the resume card: {other:?}"),
    }
}

fn kinds(s: &AppState) -> Vec<RowKind> {
    s.home.rows.iter().map(|r| r.kind).collect()
}

fn places(s: &AppState) -> Vec<Option<String>> {
    s.multiview
        .as_ref()
        .expect("multiview runs")
        .tiles
        .iter()
        .map(|t| t.as_ref().map(|t| t.stream.channel.login.clone()))
        .collect()
}

fn saves(cmds: &[Command]) -> Vec<SavedSet> {
    cmds.iter()
        .filter_map(|c| match c {
            Command::SaveMultiview(set) => Some(set.clone()),
            _ => None,
        })
        .collect()
}

fn played(cmds: &[Command]) -> Vec<String> {
    cmds.iter()
        .filter_map(|c| match c {
            Command::Play { channel, .. } => Some(channel.login.clone()),
            _ => None,
        })
        .collect()
}

/// Down to the first row of cards, A on the resume card.
fn start(s: &AppState) -> (AppState, Vec<Command>) {
    press(s, &[Intent::Down, Intent::Select])
}

// --- The card on Home ----------------------------------------------------------------------------

#[test]
fn the_card_opens_the_first_row_of_streams_with_the_live_ones() {
    let (s, cmds) = home_with(vec![hero(), top_live(), checked()]);
    assert_eq!(kinds(&s), [RowKind::Featured, RowKind::TopLive]);
    assert_eq!(card_logins(s.home.tile(1, 0)), ["a", "c", "e"]);
    assert_eq!(s.home.row_len(1), Some(6));
    // Its mosaic is fetched with the other thumbnails.
    let urls: Vec<&String> = cmds
        .iter()
        .filter_map(|c| match c {
            Command::FetchThumbnails { urls } => Some(urls),
            _ => None,
        })
        .flatten()
        .collect();
    for login in ["a", "c", "e"] {
        assert!(
            urls.contains(&&format!("https://img/{login}.jpg")),
            "{urls:?}"
        );
    }
}

#[test]
fn the_card_is_hidden_with_fewer_than_two_live() {
    let one = Row::new(RowKind::Resume, vec![Tile::Stream(live("a")), offline("b")]);
    let (s, _) = home_with(vec![hero(), top_live(), one]);
    assert_eq!(kinds(&s), [RowKind::Featured, RowKind::TopLive]);
    assert!(s
        .home
        .rows
        .iter()
        .flat_map(|r| &r.tiles)
        .all(|t| !matches!(t, Tile::Resume(_))));
}

#[test]
fn without_a_row_of_streams_the_card_has_a_row_of_its_own_after_the_hero() {
    let categories = Row::new(
        RowKind::Categories,
        vec![Tile::Category(Category {
            id: "1".into(),
            name: "Chess".into(),
            tint: Rgb::default(),
            thumbnail: None,
        })],
    );
    let (s, _) = home_with(vec![hero(), categories, checked()]);
    assert_eq!(
        kinds(&s),
        [RowKind::Featured, RowKind::Resume, RowKind::Categories]
    );
    assert_eq!(s.home.row_len(1), Some(1));
}

#[test]
fn signed_out_there_is_no_card() {
    let (mut s, _) = init();
    s.last_multiview = Some(saved());
    let (s, _) = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![hero(), top_live(), checked()],
        },
    );
    assert_eq!(s.home.row_len(1), Some(5));

    // Signing out takes it away at once.
    let (s, _) = reduce(&home(), Event::SignedOut);
    assert!(!matches!(s.home.tile(1, 0), Some(Tile::Resume(_))));
}

#[test]
fn the_card_follows_a_new_first_row_and_focus_stays_on_its_card() {
    // Focus on the third stream of Top Live (the card is at column 0).
    let (s, _) = press(
        &home(),
        &[Intent::Down, Intent::Right, Intent::Right, Intent::Right],
    );
    assert_eq!(s.home.focus, Focus { row: 1, col: 3 });
    let focused = s.home.focused_tile().cloned();
    // Starring it adds a row of live favorites before Top Live: the card moves there.
    let (mut s, _) = reduce(&s, Intent::View);
    let popup = s.popup.as_mut().unwrap();
    popup.focus = popup
        .rows
        .iter()
        .position(|r| *r == PopupRow::Favorite)
        .unwrap();
    let (s, _) = reduce(&s, Intent::Select);
    assert_eq!(
        kinds(&s),
        [RowKind::Featured, RowKind::FavoritesLive, RowKind::TopLive]
    );
    assert_eq!(card_logins(s.home.tile(1, 0)), ["a", "c", "e"]);
    assert_eq!(s.home.row_len(2), Some(5), "Top Live lost the card");
    assert_eq!(s.home.focused_tile().cloned(), focused);
}

// --- Starting the set ----------------------------------------------------------------------------

#[test]
fn a_on_the_card_starts_the_live_ones_in_the_saved_layout_and_sound() {
    let (s, cmds) = start(&home());
    assert_eq!(s.screen, Screen::Multiview);
    let mv = s.multiview.as_ref().unwrap();
    assert_eq!(mv.layout, Layout::Grid(6));
    assert_eq!(
        places(&s),
        [
            Some("a".into()),
            Some("c".into()),
            Some("e".into()),
            None,
            None,
            None
        ]
    );
    // `c` had the sound, and loads first.
    assert_eq!(mv.sound, 1);
    assert_eq!(mv.focus, 1);
    assert_eq!(s.now_playing.as_ref().unwrap().channel.login, "c");
    assert_eq!(played(&cmds), ["c", "a", "e"]);
    assert_eq!(s.back_stack.last(), Some(&Screen::Home));
}

#[test]
fn offline_channels_are_skipped_with_a_toast() {
    let (s, cmds) = start(&home());
    assert_eq!(s.toasts.len(), 1);
    assert_eq!(s.toasts[0].kind, ToastKind::OfflineSkipped(2));
    assert!(cmds.contains(&Command::ExpireToast { id: s.toasts[0].id }));
}

#[test]
fn nothing_skipped_no_toast() {
    let all_live = Row::new(
        RowKind::Resume,
        ["a", "b", "c", "d", "e"]
            .into_iter()
            .map(|l| Tile::Stream(live(l)))
            .collect(),
    );
    let (s, _) = home_with(vec![hero(), top_live(), all_live]);
    let (s, _) = start(&s);
    assert_eq!(places(&s).iter().flatten().count(), 5);
    assert!(s.toasts.is_empty());
}

#[test]
fn a_saved_layout_too_small_for_the_live_ones_gives_the_smallest_grid() {
    let mut s = home();
    s.last_multiview = Some(SavedSet {
        layout: Layout::Grid(2),
        ..saved()
    });
    let (s, _) = start(&s);
    assert_eq!(s.multiview.as_ref().unwrap().layout, Layout::Grid(3));
}

// --- Saving --------------------------------------------------------------------------------------

#[test]
fn starting_saves_the_set_that_runs() {
    let (s, cmds) = start(&home());
    let want = SavedSet {
        channels: ["a", "c", "e"]
            .into_iter()
            .map(ChannelRef::twitch)
            .collect(),
        layout: Layout::Grid(6),
        sound: 1,
    };
    assert_eq!(saves(&cmds), std::slice::from_ref(&want));
    assert_eq!(s.last_multiview, Some(want));
}

#[test]
fn moving_the_sound_or_the_layout_saves_and_moving_focus_does_not() {
    let (s, _) = start(&home());
    // Focus to `a` and listen there.
    let (s, cmds) = press(&s, &[Intent::Left]);
    assert!(saves(&cmds).is_empty());
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(saves(&cmds).len(), 1);
    assert_eq!(s.last_multiview.as_ref().unwrap().sound, 0);
    let (s, cmds) = reduce(&s, Intent::PageRight);
    assert_eq!(saves(&cmds)[0].layout, s.multiview.as_ref().unwrap().layout);
}

#[test]
fn leaving_keeps_the_saved_set_and_shows_it_on_the_card() {
    let (s, _) = start(&home());
    // Listen to `a`, then leave.
    let (s, _) = press(&s, &[Intent::Left, Intent::Select]);
    let before = s.last_multiview.clone();
    let (s, cmds) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Home);
    assert!(saves(&cmds).is_empty());
    assert_eq!(s.last_multiview, before);
    // The card shows the set just left, in place order.
    assert_eq!(card_logins(s.home.tile(1, 0)), ["a", "c", "e"]);
    assert_eq!(s.home.row_len(1), Some(6), "still one card");
}

#[test]
fn a_set_left_with_one_live_stream_takes_the_card_away() {
    let (s, _) = start(&home());
    // `c` and `e` go offline.
    let (s, _) = run(
        &s,
        vec![
            Event::StreamOffline {
                channel: ChannelRef::twitch("c"),
            }
            .into(),
            Event::StreamOffline {
                channel: ChannelRef::twitch("e"),
            }
            .into(),
            Intent::Back.into(),
        ],
    );
    assert_eq!(s.screen, Screen::Home);
    assert!(!matches!(s.home.tile(1, 0), Some(Tile::Resume(_))));
    assert_eq!(s.home.row_len(1), Some(5));
}
