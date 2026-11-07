//! Favorites (plan.md, phase 6): starring and unstarring from the card popup, the player and
//! multiview, the Favorites screen and rail entry, Home's row, the picker's Favorites tab, and
//! saving the list.

use htpc_core::*;

fn live(login: &str, viewers: u64) -> Stream {
    Stream {
        channel: ChannelRef::twitch(login),
        user_id: format!("id_{login}"),
        title: format!("{login} title"),
        category: "Cat".into(),
        viewers,
        tint: Rgb::default(),
        thumbnail: None,
    }
}

fn offline(channel: ChannelRef) -> Tile {
    Tile::Channel(Channel {
        name: channel.login.clone(),
        channel,
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

fn sign_in(s: &AppState) -> AppState {
    let user = User {
        login: "viewer".into(),
        can_chat: true,
    };
    reduce(s, Event::SignedIn(user)).0
}

/// Home: the hero, Followed (a0, a1), Top Live (t0, t1, t2), and an offline followed channel.
fn home_rows() -> Vec<Row> {
    vec![
        Row::new(RowKind::Featured, vec![Tile::Stream(live("hero", 9_000))]),
        Row::new(
            RowKind::FollowedLive,
            vec![Tile::Stream(live("a0", 500)), Tile::Stream(live("a1", 400))],
        ),
        Row::new(
            RowKind::TopLive,
            (0..3)
                .map(|i| Tile::Stream(live(&format!("t{i}"), 100)))
                .collect(),
        ),
        Row::new(
            RowKind::FollowedOffline,
            vec![offline(ChannelRef::twitch("sleepy"))],
        ),
    ]
}

/// Signed in on a loaded Home.
fn home() -> AppState {
    let (s, _) = init();
    let s = sign_in(&s);
    let rows = home_rows();
    reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    )
    .0
}

fn saved(cmds: &[Command]) -> Option<Vec<ChannelRef>> {
    cmds.iter().rev().find_map(|c| match c {
        Command::SaveFavorites(list) => Some(list.clone()),
        _ => None,
    })
}

/// View on the focused card (or stream), then A on the Favorite row.
fn star(s: &AppState) -> (AppState, Vec<Command>) {
    let (mut s, _) = reduce(s, Intent::View);
    let popup = s.popup.as_mut().expect("a popup");
    popup.focus = popup
        .rows
        .iter()
        .position(|r| *r == PopupRow::Favorite)
        .unwrap_or_else(|| panic!("no Favorite row in {:?}", popup.rows));
    reduce(&s, Intent::Select)
}

fn kinds(feed: &Feed) -> Vec<RowKind> {
    feed.rows.iter().map(|r| r.kind).collect()
}

fn keys(row: &Row) -> Vec<String> {
    row.tiles.iter().map(|t| t.key().1.to_owned()).collect()
}

// --- Starring ------------------------------------------------------------------------------------

#[test]
fn starring_a_live_card_saves_it_and_adds_it_to_the_live_rows() {
    // Down to Followed, a0.
    let (s, _) = press(&home(), &[Intent::Down]);
    let (s, cmds) = star(&s);
    assert!(s.popup.is_none());
    assert_eq!(s.favorites, [ChannelRef::twitch("a0")]);
    assert_eq!(saved(&cmds), Some(vec![ChannelRef::twitch("a0")]));
    // Home gains its favorites row after Followed, and focus stays on a0.
    assert_eq!(
        kinds(&s.home),
        [
            RowKind::Featured,
            RowKind::FollowedLive,
            RowKind::FavoritesLive,
            RowKind::TopLive,
            RowKind::FollowedOffline
        ]
    );
    assert_eq!(keys(&s.home.rows[2]), ["a0"]);
    assert_eq!(s.home.focus, Focus { row: 1, col: 0 });
    assert_eq!(kinds(&s.favorites_feed), [RowKind::FavoritesLive]);
}

#[test]
fn starring_a_row_above_the_focus_keeps_the_focus_on_its_card() {
    // Focus on Top Live's t1, then star it: the new row goes above it.
    let (s, _) = press(&home(), &[Intent::Down, Intent::Down, Intent::Right]);
    let (s, _) = star(&s);
    assert_eq!(s.home.rows[2].kind, RowKind::FavoritesLive);
    let focused = s.focused_tile().and_then(Tile::channel).cloned();
    assert_eq!(focused, Some(ChannelRef::twitch("t1")));
}

#[test]
fn the_popup_row_says_whether_the_channel_is_a_favorite_and_toggles() {
    let (s, _) = press(&home(), &[Intent::Down]);
    let (s, _) = star(&s);
    assert!(s.is_favorite(&ChannelRef::twitch("a0")));
    // A second time removes it, from the list and from both rows.
    let (s, cmds) = star(&s);
    assert!(s.favorites.is_empty());
    assert_eq!(saved(&cmds), Some(Vec::new()));
    assert!(s.home.rows.iter().all(|r| r.kind != RowKind::FavoritesLive));
    assert!(s.favorites_feed.rows.is_empty());
    assert_eq!(s.home.focus, Focus { row: 1, col: 0 });
}

#[test]
fn an_offline_card_offers_only_the_star_and_joins_the_offline_row() {
    let (s, _) = press(&home(), &[Intent::Down, Intent::Down, Intent::Down]);
    let (open, _) = reduce(&s, Intent::View);
    assert_eq!(open.popup.as_ref().unwrap().rows, [PopupRow::Favorite]);
    let (s, cmds) = reduce(&open, Intent::Select);
    assert_eq!(saved(&cmds), Some(vec![ChannelRef::twitch("sleepy")]));
    assert_eq!(kinds(&s.favorites_feed), [RowKind::FavoritesOffline]);
    // Not live, so not on Home's row.
    assert!(s.home.rows.iter().all(|r| r.kind != RowKind::FavoritesLive));
}

#[test]
fn signed_out_nothing_can_be_starred() {
    let (s, _) = init();
    let (s, _) = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: home_rows(),
        },
    );
    let (s, _) = press(
        &s,
        &[Intent::Down, Intent::Down, Intent::Down, Intent::View],
    );
    assert_eq!(s.popup.as_ref().unwrap().rows, [PopupRow::SignIn]);
    let (s, cmds) = reduce(&s, Intent::Select);
    assert!(s.favorites.is_empty());
    assert_eq!(saved(&cmds), None);
    assert_eq!(s.screen, Screen::Login);
}

#[test]
fn the_single_player_stars_the_stream_on_screen() {
    let (s, _) = press(&home(), &[Intent::Select]);
    let (s, _) = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT });
    let (s, cmds) = star(&s);
    assert_eq!(saved(&cmds), Some(vec![ChannelRef::twitch("hero")]));
    assert_eq!(s.screen, Screen::Player, "the stream keeps playing");
    assert_eq!(kinds(&s.favorites_feed), [RowKind::FavoritesLive]);
}

#[test]
fn an_offline_stream_on_screen_joins_the_offline_row() {
    let (s, _) = press(&home(), &[Intent::Select]);
    let (s, _) = reduce(
        &s,
        Event::StreamOffline {
            channel: ChannelRef::twitch("hero"),
        },
    );
    let (s, _) = star(&s);
    assert_eq!(kinds(&s.favorites_feed), [RowKind::FavoritesOffline]);
}

#[test]
fn multiview_stars_the_focused_tile() {
    // a0 and a1 together, then View on a1.
    let (s, _) = press(&home(), &[Intent::Down]);
    let (mut s, _) = reduce(&s, Intent::View);
    s.popup.as_mut().unwrap().focus = 0; // Pick a0
    let (s, _) = reduce(&s, Intent::Select);
    let (mut s, _) = press(&s, &[Intent::Right, Intent::View]);
    let popup = s.popup.as_mut().unwrap();
    popup.focus = popup
        .rows
        .iter()
        .position(|r| *r == PopupRow::WatchPicked)
        .unwrap();
    let (s, _) = reduce(&s, Intent::Select);
    assert_eq!(s.screen, Screen::Multiview);
    let (s, _) = reduce(&s, Intent::Right);
    let (s, cmds) = star(&s);
    assert_eq!(saved(&cmds), Some(vec![ChannelRef::twitch("a1")]));
    assert_eq!(s.screen, Screen::Multiview, "multiview carries on");
    assert!(cmds.iter().all(|c| !matches!(c, Command::Play { .. })));
}

// --- The Favorites screen ------------------------------------------------------------------------

#[test]
fn the_rail_entry_sits_after_following() {
    assert_eq!(RailEntry::Following.next(), RailEntry::Favorites);
    assert_eq!(RailEntry::Favorites.next(), RailEntry::Browse);
    assert!(Screen::Favorites.is_root());
    assert!(Screen::Favorites.needs_account());
}

#[test]
fn signed_out_the_entry_signs_in_first_then_opens_favorites() {
    let (s, _) = init();
    let (s, cmds) = reduce(&s, Intent::RailPick(RailEntry::Favorites));
    assert_eq!(s.screen, Screen::Login);
    assert_eq!(cmds, [Command::StartLogin]);
    let s = AppState {
        favorites: vec![ChannelRef::twitch("a0")],
        ..s
    };
    let (s, cmds) = reduce(
        &s,
        Event::SignedIn(User {
            login: "viewer".into(),
            can_chat: true,
        }),
    );
    assert_eq!(s.screen, Screen::Favorites);
    assert_eq!(s.rail.index, RailEntry::Favorites);
    assert!(cmds.contains(&Command::LoadFeed {
        feed: FeedId::Favorites
    }));
}

#[test]
fn opening_favorites_asks_who_is_live_and_lists_live_then_offline() {
    let s = AppState {
        favorites: vec![ChannelRef::twitch("a0"), ChannelRef::kick("napper")],
        ..home()
    };
    let (s, cmds) = reduce(&s, Intent::RailPick(RailEntry::Favorites));
    assert_eq!(s.screen, Screen::Favorites);
    assert!(s.back_stack.is_empty());
    assert!(s.favorites_feed.loading);
    assert_eq!(
        cmds,
        [Command::LoadFeed {
            feed: FeedId::Favorites
        }]
    );
    let (s, _) = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Favorites,
            rows: vec![
                Row::new(RowKind::FavoritesLive, vec![Tile::Stream(live("a0", 500))]),
                Row::new(
                    RowKind::FavoritesOffline,
                    vec![offline(ChannelRef::kick("napper"))],
                ),
            ],
        },
    );
    assert_eq!(s.current_feed_id(), Some(FeedId::Favorites));
    assert_eq!(s.row_len(0), Some(1));
    // A on the offline one tries to play it, like Following's offline cards.
    let (s, cmds) = press(&s, &[Intent::Down, Intent::Select]);
    assert_eq!(s.screen, Screen::Player);
    assert!(cmds
        .iter()
        .any(|c| matches!(c, Command::Play { channel, .. }
        if *channel == ChannelRef::kick("napper"))));
}

#[test]
fn no_favorites_asks_nothing() {
    let (s, cmds) = reduce(&home(), Intent::RailPick(RailEntry::Favorites));
    assert_eq!(s.screen, Screen::Favorites);
    assert!(cmds.is_empty());
    assert!(!s.favorites_feed.loading);
    assert!(s.favorites_feed.rows.is_empty());
}

#[test]
fn unstarring_on_the_favorites_screen_takes_the_card_away() {
    let s = AppState {
        favorites: vec![ChannelRef::twitch("a0"), ChannelRef::twitch("a1")],
        ..home()
    };
    let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Favorites));
    let (s, _) = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Favorites,
            rows: vec![Row::new(
                RowKind::FavoritesLive,
                vec![Tile::Stream(live("a0", 500)), Tile::Stream(live("a1", 400))],
            )],
        },
    );
    let (s, _) = press(&s, &[Intent::Right]);
    let (s, cmds) = star(&s);
    assert_eq!(saved(&cmds), Some(vec![ChannelRef::twitch("a0")]));
    assert_eq!(keys(&s.favorites_feed.rows[0]), ["a0"]);
    assert_eq!(s.favorites_feed.focus, Focus { row: 0, col: 0 });
}

#[test]
fn a_load_that_crossed_an_unstar_leaves_the_channel_out() {
    let s = AppState {
        favorites: vec![ChannelRef::twitch("a0")],
        ..home()
    };
    let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Favorites));
    // Unstarred from the single player while the answer was on its way: here, by hand.
    let s = AppState {
        favorites: Vec::new(),
        ..s
    };
    let (s, _) = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Favorites,
            rows: vec![Row::new(
                RowKind::FavoritesLive,
                vec![Tile::Stream(live("a0", 500))],
            )],
        },
    );
    assert!(s.favorites_feed.rows.is_empty());
    // Home's row too.
    let mut rows = home_rows();
    rows.insert(
        2,
        Row::new(RowKind::FavoritesLive, vec![Tile::Stream(live("a0", 500))]),
    );
    let (s, _) = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    );
    assert!(s.home.rows.iter().all(|r| r.kind != RowKind::FavoritesLive));
}

#[test]
fn signing_out_leaves_favorites_for_home_and_keeps_the_list() {
    let s = AppState {
        favorites: vec![ChannelRef::twitch("a0")],
        ..home()
    };
    let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Favorites));
    let (s, cmds) = reduce(&s, Event::SignedOut);
    assert_eq!(s.screen, Screen::Home);
    assert_eq!(s.favorites, [ChannelRef::twitch("a0")]);
    assert_eq!(saved(&cmds), None, "nothing to save");
    assert!(s.favorites_feed.rows.is_empty());
}

// --- The picker ----------------------------------------------------------------------------------

/// Multiview with `a0` (from the single player), the picker open, favorites `fav` live with
/// `on` live and the rest offline, and Following holding `a1`.
fn picker(fav: &[&str], on: &[&str]) -> AppState {
    let s = AppState {
        favorites: fav.iter().map(|l| ChannelRef::twitch(*l)).collect(),
        ..home()
    };
    let (s, _) = press(&s, &[Intent::Down, Intent::Select]);
    let (s, _) = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT });
    // View, *Add a stream to watch with this one*.
    let (s, cmds) = press(&s, &[Intent::View, Intent::Select]);
    assert_eq!(s.screen, Screen::Multiview);
    assert!(
        cmds.contains(&Command::LoadFeed {
            feed: FeedId::Favorites
        }) == !fav.is_empty()
    );
    let live_rows: Vec<Tile> = on.iter().map(|l| Tile::Stream(live(l, 100))).collect();
    let off: Vec<Tile> = fav
        .iter()
        .filter(|l| !on.contains(l))
        .map(|l| offline(ChannelRef::twitch(*l)))
        .collect();
    let mut rows = Vec::new();
    if !live_rows.is_empty() {
        rows.push(Row::new(RowKind::FavoritesLive, live_rows));
    }
    if !off.is_empty() {
        rows.push(Row::new(RowKind::FavoritesOffline, off));
    }
    run(
        &s,
        vec![
            Event::FeedLoaded {
                feed: FeedId::Favorites,
                rows,
            }
            .into(),
            Event::FeedLoaded {
                feed: FeedId::Following,
                rows: vec![Row::new(
                    RowKind::FollowedLive,
                    vec![Tile::Stream(live("a1", 400))],
                )],
            }
            .into(),
        ],
    )
    .0
}

fn picker_state(s: &AppState) -> Picker {
    s.multiview.as_ref().unwrap().picker.expect("the picker")
}

#[test]
fn the_picker_opens_on_favorites_when_there_are_any() {
    assert_eq!(picker_state(&picker(&[], &[])).tab, PickerTab::Following);
    let s = picker(&["f0", "f1"], &["f1"]);
    assert_eq!(picker_state(&s).tab, PickerTab::Favorites);
    // Live first, then offline.
    let listed: Vec<&str> = s
        .picker_rows(PickerTab::Favorites)
        .iter()
        .map(|t| t.key().1)
        .collect();
    assert_eq!(listed, ["f1", "f0"]);
}

#[test]
fn a_on_an_offline_favorite_adds_nothing() {
    let s = picker(&["f0", "f1"], &["f1"]);
    let (s, cmds) = press(&s, &[Intent::Down, Intent::Select]);
    assert!(cmds.iter().all(|c| !matches!(c, Command::Play { .. })));
    assert_eq!(s.multiview.as_ref().unwrap().count(), 1);
    // The live one is added.
    let (s, cmds) = press(&s, &[Intent::Up, Intent::Select]);
    assert!(cmds
        .iter()
        .any(|c| matches!(c, Command::Play { channel, .. }
        if *channel == ChannelRef::twitch("f1"))));
    assert_eq!(s.multiview.as_ref().unwrap().count(), 2);
}

#[test]
fn y_in_the_picker_stars_and_unstars() {
    // On Following, Y stars a1.
    let s = picker(&["f0"], &[]);
    let (s, cmds) = press(&s, &[Intent::PageRight, Intent::ToggleChat]);
    assert_eq!(picker_state(&s).tab, PickerTab::Following);
    assert_eq!(
        saved(&cmds),
        Some(vec![ChannelRef::twitch("f0"), ChannelRef::twitch("a1")])
    );
    assert!(s.chat_open == home().chat_open, "Y is not chat here");
    // Back on Favorites, a1 is listed live first; Y on the last row unstars f0 and the focus
    // stays inside the list.
    let (s, _) = press(&s, &[Intent::PageLeft]);
    let listed: Vec<&str> = s
        .picker_rows(PickerTab::Favorites)
        .iter()
        .map(|t| t.key().1)
        .collect();
    assert_eq!(listed, ["a1", "f0"]);
    let (s, cmds) = press(&s, &[Intent::Down, Intent::ToggleChat]);
    assert_eq!(saved(&cmds), Some(vec![ChannelRef::twitch("a1")]));
    assert_eq!(s.picker_rows(PickerTab::Favorites).len(), 1);
    assert_eq!(picker_state(&s).focus, 0);
}
