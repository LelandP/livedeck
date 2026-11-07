//! Picking streams for multiview on the browse screens (plan.md, section 2 and phase 5): the card
//! popup's rows, the picked set and its limit, *Watch the N together*, B keeping the set picked,
//! and *Watch with the N picked* on the single player.

use htpc_core::*;

fn live(login: &str) -> Stream {
    Stream {
        channel: ChannelRef::twitch(login),
        user_id: format!("id_{login}"),
        title: format!("{login} title"),
        category: "Cat".into(),
        viewers: 1_000,
        tint: Rgb::default(),
        thumbnail: None,
    }
}

const CATEGORY: &str = "509658";

/// Ten live streams `s0` ... `s9`.
fn streams() -> Vec<Tile> {
    (0..10)
        .map(|i| Tile::Stream(live(&format!("s{i}"))))
        .collect()
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

/// Home: a row of ten live streams, a category, and an offline channel.
fn home(signed_in: bool) -> AppState {
    let (s, _) = init();
    let category = Tile::Category(Category {
        id: CATEGORY.into(),
        name: "Just Chatting".into(),
        tint: Rgb::default(),
        thumbnail: None,
    });
    let offline = Tile::Channel(Channel {
        channel: ChannelRef::twitch("sleepy"),
        name: "sleepy".into(),
        tint: Rgb::default(),
    });
    let mut msgs: Vec<Msg> = vec![Event::FeedLoaded {
        feed: FeedId::Home,
        rows: vec![
            Row::new(RowKind::TopLive, streams()),
            Row::new(RowKind::Categories, vec![category]),
            Row::new(RowKind::FollowedOffline, vec![offline]),
        ],
    }
    .into()];
    if signed_in {
        msgs.push(
            Event::SignedIn(User {
                login: "viewer".into(),
                can_chat: true,
            })
            .into(),
        );
    }
    run(&s, msgs).0
}

fn rows(s: &AppState) -> Vec<PopupRow> {
    s.popup.as_ref().expect("a popup").rows.clone()
}

/// View on the focused card, then A on `row`.
fn choose(s: &AppState, row: PopupRow) -> (AppState, Vec<Command>) {
    let (mut s, _) = reduce(s, Intent::View);
    let popup = s.popup.as_mut().expect("a popup");
    popup.focus = popup
        .rows
        .iter()
        .position(|r| *r == row)
        .unwrap_or_else(|| panic!("no {row:?} in {:?}", popup.rows));
    reduce(&s, Intent::Select)
}

/// Picks the focused card and moves one card to the right.
fn pick_and_next(s: &AppState) -> AppState {
    let (s, _) = choose(s, PopupRow::Pick);
    reduce(&s, Intent::Right).0
}

/// The first `n` cards picked, focus on the card after them.
fn picked(n: usize) -> AppState {
    (0..n).fold(home(true), |s, _| pick_and_next(&s))
}

fn logins(streams: &[Stream]) -> Vec<String> {
    streams.iter().map(|s| s.channel.login.clone()).collect()
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

fn plays(cmds: &[Command]) -> Vec<Slot> {
    cmds.iter()
        .filter_map(|c| match c {
            Command::Play { slot, .. } => Some(*slot),
            _ => None,
        })
        .collect()
}

// --- The card popup ------------------------------------------------------------------------------

#[test]
fn signed_out_a_card_only_offers_to_sign_in_and_picks_nothing() {
    let (s, _) = reduce(&home(false), Intent::View);
    assert_eq!(rows(&s), [PopupRow::SignIn]);
    let (s, _) = reduce(&s, Intent::Select);
    assert!(s.picked.is_empty());
}

#[test]
fn view_on_a_card_picks_it() {
    let s = home(true);
    let (open, cmds) = reduce(&s, Intent::View);
    assert_eq!(
        rows(&open),
        [PopupRow::Pick, PopupRow::Favorite],
        "nothing picked yet"
    );
    assert!(cmds.is_empty());
    let (s, cmds) = reduce(&open, Intent::Select);
    assert_eq!(logins(&s.picked), ["s0"]);
    assert!(s.popup.is_none());
    assert!(cmds.is_empty(), "picking plays nothing");
    assert_eq!(s.screen, Screen::Home);
    assert!(s.is_picked(&ChannelRef::twitch("s0")));
}

#[test]
fn the_rows_follow_what_is_picked() {
    let s = picked(1);
    // On a picked card.
    let (on_picked, _) = press(&s, &[Intent::Left, Intent::View]);
    assert_eq!(
        rows(&on_picked),
        [PopupRow::Unpick, PopupRow::ClearPicked, PopupRow::Favorite]
    );
    // On another card: this one would make two.
    let (on_other, _) = reduce(&s, Intent::View);
    assert_eq!(
        rows(&on_other),
        [
            PopupRow::Pick,
            PopupRow::WatchPicked,
            PopupRow::ClearPicked,
            PopupRow::Favorite
        ]
    );
}

#[test]
fn unpicking_and_clearing() {
    let s = picked(3);
    let (s, _) = reduce(&s, Intent::Left);
    let (s, _) = choose(&s, PopupRow::Unpick);
    assert_eq!(logins(&s.picked), ["s0", "s1"]);
    let (s, _) = choose(&s, PopupRow::ClearPicked);
    assert!(s.picked.is_empty());
}

#[test]
fn at_most_nine_are_picked() {
    let s = picked(9);
    assert_eq!(s.picked.len(), 9);
    // The tenth card cannot be added, but the nine can be watched.
    let (open, _) = reduce(&s, Intent::View);
    assert_eq!(
        rows(&open),
        [
            PopupRow::WatchPicked,
            PopupRow::ClearPicked,
            PopupRow::Favorite
        ]
    );
    assert_eq!(
        s.watch_set(s.focused_tile().and_then(|t| match t {
            Tile::Stream(st) => Some(st),
            _ => None,
        }))
        .len(),
        9
    );
}

#[test]
fn category_cards_are_ignored_and_offline_ones_can_only_be_starred() {
    let s = picked(1);
    let (on_category, cmds) = press(&s, &[Intent::Down, Intent::View]);
    assert!(on_category.popup.is_none());
    assert!(cmds.is_empty());

    let (on_offline, cmds) = press(&s, &[Intent::Down, Intent::Down, Intent::View]);
    assert_eq!(rows(&on_offline), [PopupRow::Favorite]);
    assert!(cmds.is_empty());
    // Nothing about it can be picked or watched.
    let (s, _) = reduce(&on_offline, Intent::Select);
    assert_eq!(logins(&s.picked), ["s0"]);
    assert!(s.multiview.is_none());
    assert!(s.is_favorite(&ChannelRef::twitch("sleepy")));
}

#[test]
fn signing_out_forgets_the_set() {
    let s = picked(2);
    let (s, _) = reduce(&s, Event::SignedOut);
    assert!(s.picked.is_empty());
}

// --- Watching the set ----------------------------------------------------------------------------

#[test]
fn watch_together_starts_the_smallest_grid_with_sound_on_the_first() {
    // s0 and s1 picked, View on s2.
    let s = picked(2);
    let (s, cmds) = choose(&s, PopupRow::WatchPicked);

    assert_eq!(s.screen, Screen::Multiview);
    assert_eq!(s.back_stack, [Screen::Home]);
    let mv = s.multiview.as_ref().unwrap();
    assert_eq!(mv.layout, Layout::Grid(3));
    assert_eq!(
        places(&s),
        [Some("s0".into()), Some("s1".into()), Some("s2".into())]
    );
    assert_eq!(mv.sound, 0);
    assert_eq!(s.sound_slot(), 0);
    assert_eq!(s.now_playing.as_ref().unwrap().channel.login, "s0");
    assert_eq!(s.playback, Playback::Loading);
    assert!(mv.picker.is_none());

    // Every stream loads on its own slot at its tile's quality, all but the first muted.
    assert_eq!(plays(&cmds), [0, 1, 2]);
    for slot in [1, 2] {
        assert!(cmds.contains(&Command::SetMuted { slot, muted: true }));
    }
    assert!(cmds.contains(&Command::SetMuted {
        slot: 0,
        muted: false
    }));
    assert!(cmds.contains(&Command::Play {
        slot: 2,
        channel: ChannelRef::twitch("s2"),
        stream: StreamKind::Tile {
            rendition: Rendition::P360,
            rate: FrameRate::Light
        },
        latency: Latency::Low,
    }));
    // The first one's chat, raids and hype train.
    assert!(cmds.contains(&Command::JoinChat {
        channel: ChannelRef::twitch("s0")
    }));
    assert!(cmds.contains(&Command::WatchRaids {
        broadcaster_id: "id_s0".into()
    }));
    // The card it was started from joined the set.
    assert_eq!(logins(&s.picked), ["s0", "s1", "s2"]);

    // The players report in.
    let (s, _) = run(
        &s,
        (0..3)
            .map(|slot| Event::PlaybackStarted { slot }.into())
            .collect(),
    );
    assert_eq!(s.playback, Playback::Playing);
}

#[test]
fn watch_together_on_a_picked_card_starts_the_picked_set() {
    let s = picked(2);
    let (s, _) = press(&s, &[Intent::Left]);
    let (s, _) = choose(&s, PopupRow::WatchPicked);
    assert_eq!(places(&s), [Some("s0".into()), Some("s1".into())]);
    assert_eq!(s.multiview.as_ref().unwrap().layout, Layout::Grid(2));
}

#[test]
fn nine_together_fill_the_grid_of_nine() {
    let s = picked(9);
    let (s, cmds) = choose(&s, PopupRow::WatchPicked);
    let mv = s.multiview.as_ref().unwrap();
    assert_eq!(mv.layout, Layout::Grid(9));
    assert_eq!(mv.count(), 9);
    assert_eq!(plays(&cmds), (0..9).collect::<Vec<Slot>>());
}

#[test]
fn b_returns_to_the_screen_it_was_started_from_with_the_set_still_picked() {
    let s = picked(2);
    let (s, _) = choose(&s, PopupRow::WatchPicked);
    // The sound moves to s1, then B.
    let (s, _) = press(&s, &[Intent::Right, Intent::Select]);
    assert_eq!(s.now_playing.as_ref().unwrap().channel.login, "s1");
    let (s, cmds) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Home);
    assert!(s.multiview.is_none());
    for slot in 0..3 {
        assert!(cmds.contains(&Command::StopPlayback { slot }));
    }
    // The one with sound first, so starting again gives it the sound.
    assert_eq!(logins(&s.picked), ["s1", "s0", "s2"]);
    // The focus is where it was: on s2, now picked.
    let (open, _) = reduce(&s, Intent::View);
    assert_eq!(
        rows(&open),
        [
            PopupRow::Unpick,
            PopupRow::WatchPicked,
            PopupRow::ClearPicked,
            PopupRow::Favorite
        ]
    );
    let (again, _) = choose(&s, PopupRow::WatchPicked);
    assert_eq!(again.now_playing.as_ref().unwrap().channel.login, "s1");
}

#[test]
fn leaving_drops_offline_streams_from_the_set() {
    let s = picked(2);
    let (s, _) = choose(&s, PopupRow::WatchPicked);
    let (s, _) = reduce(
        &s,
        Event::StreamOffline {
            channel: ChannelRef::twitch("s1"),
        },
    );
    let (s, _) = reduce(&s, Intent::Back);
    assert_eq!(logins(&s.picked), ["s0", "s2"]);
}

#[test]
fn b_returns_to_a_category() {
    let s = picked(1);
    // Down to the category, A, and its streams arrive.
    let (s, _) = press(&s, &[Intent::Down, Intent::Select]);
    assert_eq!(s.screen, Screen::Category);
    let (s, _) = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Category(CATEGORY.into()),
            rows: grid_rows(
                RowKind::CategoryStreams,
                vec![Tile::Stream(live("c0"))],
                Paging::End,
            ),
        },
    );
    let (s, _) = choose(&s, PopupRow::WatchPicked);
    assert_eq!(places(&s), [Some("s0".into()), Some("c0".into())]);
    assert_eq!(s.back_stack, [Screen::Home, Screen::Category]);
    let (s, _) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Category);
    assert!(s.category.is_some());
}

// --- The single player ---------------------------------------------------------------------------

#[test]
fn the_single_player_offers_to_watch_with_the_picked() {
    // s0 and s1 picked, then s2 opened.
    let s = picked(2);
    let (s, _) = reduce(&s, Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    let (open, _) = reduce(&s, Intent::View);
    assert_eq!(
        rows(&open),
        [
            PopupRow::AddStream,
            PopupRow::WatchPicked,
            PopupRow::Favorite
        ]
    );

    let (s, cmds) = press(&open, &[Intent::Down, Intent::Select]);
    assert_eq!(s.screen, Screen::Multiview);
    assert_eq!(
        places(&s),
        [Some("s2".into()), Some("s0".into()), Some("s1".into())]
    );
    let mv = s.multiview.as_ref().unwrap();
    assert_eq!(mv.layout, Layout::Grid(3));
    assert_eq!(mv.sound, 0, "the stream on screen keeps the sound");
    assert_eq!(mv.focus, 0);
    assert!(mv.picker.is_none());
    assert_eq!(s.playback, Playback::Playing, "and keeps playing");
    assert_eq!(plays(&cmds), [1, 2], "only the picked ones load");
    // B returns to browsing, as from the single player.
    let (s, _) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Home);
    assert_eq!(logins(&s.picked), ["s2", "s0", "s1"]);
}

#[test]
fn the_stream_on_screen_counts_once() {
    // Only s0 is picked, and s0 is on screen: nothing to watch it with.
    let s = picked(1);
    let (s, _) = press(&s, &[Intent::Left, Intent::Select]);
    let (open, _) = reduce(&s, Intent::View);
    assert_eq!(rows(&open), [PopupRow::AddStream, PopupRow::Favorite]);
}
