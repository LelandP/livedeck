//! Multiview (plan.md, section 2): the View popup, starting from the single player, the picker,
//! the grid, full screen, moving the sound, adding and removing, layouts, failed tiles, raids,
//! chat, and the quality settle timer.
//!
//! The display is never reported here, so it counts as the Deck's 1280x800: grid tiles want
//! 360p at 30 fps first, full screen 720p at 60 fps first (plan.md, section 3).

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

/// The followed channels the picker lists, in order.
const FOLLOWED: [&str; 9] = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];

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

/// Home with a live stream `main` and a category, signed in or not. Signed in, Following holds
/// the live channels of `FOLLOWED`.
fn home(signed_in: bool) -> AppState {
    let (s, _) = init();
    let category = Tile::Category(Category {
        id: "509658".into(),
        name: "Just Chatting".into(),
        tint: Rgb::default(),
        thumbnail: None,
    });
    let mut msgs: Vec<Msg> = vec![Event::FeedLoaded {
        feed: FeedId::Home,
        rows: vec![
            Row::new(RowKind::TopLive, vec![Tile::Stream(live("main"))]),
            Row::new(RowKind::Categories, vec![category]),
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
        msgs.push(
            Event::FeedLoaded {
                feed: FeedId::Following,
                rows: vec![Row::new(
                    RowKind::FollowedLive,
                    FOLLOWED.iter().map(|l| Tile::Stream(live(l))).collect(),
                )],
            }
            .into(),
        );
    }
    run(&s, msgs).0
}

/// Watching `main` in the single player, playing.
fn watching(signed_in: bool) -> AppState {
    let (s, _) = press(&home(signed_in), &[Intent::Select]);
    reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0
}

/// View, then *Add a stream to watch with this one*: the grid with `main`, the picker open.
fn started() -> (AppState, Vec<Command>) {
    press(&watching(true), &[Intent::View, Intent::Select])
}

fn mv(s: &AppState) -> &Multiview {
    s.multiview.as_ref().expect("multiview runs")
}

/// The channel at each place, `None` for an empty slot.
fn places(s: &AppState) -> Vec<Option<String>> {
    mv(s)
        .tiles
        .iter()
        .map(|t| t.as_ref().map(|t| t.stream.channel.login.clone()))
        .collect()
}

fn slot_at(s: &AppState, place: usize) -> Slot {
    mv(s).tile(place).expect("a stream there").slot
}

/// Multiview with `main` and the first `n - 1` followed channels, settled at their tile quality and
/// playing, the picker closed and the focus on `main`.
fn with_streams(n: usize) -> AppState {
    let (mut s, _) = started();
    for i in 0..n - 1 {
        if i > 0 {
            s = reduce(&s, Intent::Down).0;
        }
        s = reduce(&s, Intent::Select).0;
    }
    if mv(&s).picker.is_some() {
        s = reduce(&s, Intent::Back).0;
    }
    s = reduce(&s, Event::QualitySettleDue(s.last_settle_id)).0;
    let slots: Vec<Slot> = mv(&s).tiles.iter().flatten().map(|t| t.slot).collect();
    for slot in slots {
        s = reduce(&s, Event::PlaybackStarted { slot }).0;
    }
    s.multiview.as_mut().unwrap().focus = 0;
    s
}

fn tile_play(slot: Slot, login: &str, rendition: Rendition, rate: FrameRate) -> Command {
    Command::Play {
        slot,
        channel: ChannelRef::twitch(login),
        stream: StreamKind::Tile { rendition, rate },
        latency: Latency::Low,
    }
}

fn plays(cmds: &[Command]) -> Vec<Slot> {
    cmds.iter()
        .filter_map(|c| match c {
            Command::Play { slot, .. } => Some(*slot),
            _ => None,
        })
        .collect()
}

/// The id of the last settle timer in `cmds`.
fn settle_id(cmds: &[Command]) -> Option<u32> {
    cmds.iter().rev().find_map(|c| match c {
        Command::SettleQuality { id } => Some(*id),
        _ => None,
    })
}

// --- The View button ---------------------------------------------------------------------------

#[test]
fn the_v_key_is_the_view_button_except_while_typing() {
    let s = watching(true);
    let (s, _) = reduce(&s, Intent::Text('v'));
    assert!(s.popup.is_some(), "v opens the popup on the player");

    // On the Search keyboard it is a letter.
    let (s, _) = reduce(&home(true), Intent::RailPick(RailEntry::Search));
    let (s, _) = reduce(&s, Intent::Text('v'));
    assert_eq!(s.search.query, "v");
    assert!(s.popup.is_none());
}

#[test]
fn signed_out_the_popup_only_offers_to_sign_in() {
    // On a live stream card.
    let (s, _) = reduce(&home(false), Intent::View);
    let popup = s.popup.as_ref().expect("a popup");
    assert_eq!(popup.rows, [PopupRow::SignIn]);
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(s.screen, Screen::Login);
    assert!(s.popup.is_none());
    assert!(cmds.contains(&Command::StartLogin));

    // On the player.
    let (s, _) = reduce(&watching(false), Intent::View);
    assert_eq!(s.popup.unwrap().rows, [PopupRow::SignIn]);
}

#[test]
fn view_on_a_category_card_does_nothing() {
    for signed_in in [false, true] {
        let (s, _) = press(&home(signed_in), &[Intent::Down, Intent::View]);
        assert!(s.popup.is_none());
    }
}

#[test]
fn the_popup_closes_with_b_or_view_and_takes_the_buttons_meanwhile() {
    let s = watching(true);
    let (open, _) = reduce(&s, Intent::View);
    for close in [Intent::Back, Intent::View] {
        let (closed, cmds) = reduce(&open, close);
        assert!(closed.popup.is_none());
        assert_eq!(closed.screen, Screen::Player, "B only closes the popup");
        assert!(cmds.is_empty());
    }
    // Y does not reach the player underneath.
    let (after, _) = reduce(&open, Intent::ToggleChat);
    assert_eq!(after.chat_open, s.chat_open);
}

// --- Starting from the single player -------------------------------------------------------------

#[test]
fn adding_a_stream_to_the_single_player_starts_multiview_without_a_reload() {
    let s = watching(true);
    let (s, _) = reduce(&s, Intent::View);
    assert_eq!(
        s.popup.as_ref().unwrap().rows,
        [PopupRow::AddStream, PopupRow::Favorite]
    );
    let (s, cmds) = reduce(&s, Intent::Select);

    assert_eq!(s.screen, Screen::Multiview);
    assert_eq!(places(&s), [Some("main".into()), None]);
    assert_eq!(mv(&s).layout, Layout::Grid(2));
    assert_eq!(s.sound_slot(), MAIN_SLOT);
    assert_eq!(s.playback, Playback::Playing, "it keeps playing");
    assert!(plays(&cmds).is_empty(), "nothing is loaded again yet");
    assert!(!cmds
        .iter()
        .any(|c| matches!(c, Command::StopPlayback { .. })));
    // The picker is open, on a fresh list of who is live.
    assert!(mv(&s).picker.is_some());
    assert!(cmds.contains(&Command::LoadFeed {
        feed: FeedId::Following
    }));
    // It played at the single player's quality: smaller now, after the settle.
    let id = settle_id(&cmds).expect("a settle timer");
    let (_, cmds) = reduce(&s, Event::QualitySettleDue(id));
    assert_eq!(
        cmds,
        [tile_play(
            MAIN_SLOT,
            "main",
            Rendition::P360,
            FrameRate::Light
        )]
    );
}

#[test]
fn the_grid_replaces_the_player_so_b_returns_to_browsing() {
    let s = with_streams(2);
    assert_eq!(s.back_stack, [Screen::Home]);
    let (s, cmds) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Home);
    assert!(s.multiview.is_none());
    assert!(s.now_playing.is_none());
    assert_eq!(s.playback, Playback::Idle);
    assert!(cmds.contains(&Command::StopPlayback { slot: 0 }));
    assert!(cmds.contains(&Command::StopPlayback { slot: 1 }));
    assert!(cmds.contains(&Command::LeaveChat));
    assert!(cmds.contains(&Command::StopRaids));
}

#[test]
fn leaving_unmutes_the_single_players_slot_if_it_was_muted() {
    // The sound moved away from `main` (slot 0).
    let s = with_streams(2);
    let (s, _) = press(&s, &[Intent::Right, Intent::Select]);
    assert_eq!(s.sound_slot(), 1);
    let (_, cmds) = reduce(&s, Intent::Back);
    assert!(cmds.contains(&Command::SetMuted {
        slot: MAIN_SLOT,
        muted: false
    }));
    // With the sound on slot 0 it was never muted.
    let (_, cmds) = reduce(&with_streams(2), Intent::Back);
    assert!(!cmds.iter().any(|c| matches!(c, Command::SetMuted { .. })));
}

// --- The picker and adding -----------------------------------------------------------------------

#[test]
fn the_picker_adds_a_muted_stream_and_stays_open() {
    let (s, _) = started();
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(places(&s), [Some("main".into()), Some("a".into())]);
    assert_eq!(
        cmds[..2],
        [
            Command::SetMuted {
                slot: 1,
                muted: true
            },
            tile_play(1, "a", Rendition::P360, FrameRate::Light),
        ]
    );
    assert_eq!(s.sound_slot(), MAIN_SLOT, "the sound stays");
    assert_eq!(mv(&s).focus, 1, "focus goes to the new stream");
    assert!(mv(&s).picker.is_some());

    // A stream already watched is not added twice.
    let (mut again, cmds) = reduce(&s, Intent::Select);
    // Only the grid's controls are woken.
    again.hud_pulse = s.hud_pulse;
    assert_eq!(again, s);
    assert!(cmds.is_empty());
}

#[test]
fn a_full_grid_grows_to_the_next_size() {
    let (mut s, _) = started();
    let mut layouts = Vec::new();
    for i in 0..8 {
        if i > 0 {
            s = reduce(&s, Intent::Down).0;
        }
        s = reduce(&s, Intent::Select).0;
        layouts.push(mv(&s).layout);
    }
    assert_eq!(
        layouts,
        [
            Layout::Grid(2),
            Layout::Grid(3),
            Layout::Grid(4),
            Layout::Grid(6),
            Layout::Grid(6),
            Layout::Grid(9),
            Layout::Grid(9),
            Layout::Grid(9),
        ]
    );
    assert_eq!(mv(&s).count(), 9);
    assert!(mv(&s).picker.is_none(), "full: the picker closes");
    let slots: Vec<Slot> = mv(&s).tiles.iter().flatten().map(|t| t.slot).collect();
    assert_eq!(slots, (0..9).collect::<Vec<Slot>>(), "each its own slot");
}

#[test]
fn picker_tabs_and_back() {
    let (s, _) = started();
    let tab = mv(&s).picker.unwrap().tab;
    assert_eq!(tab, PickerTab::Following, "no favorites yet");
    let (s, _) = reduce(&s, Intent::PageRight);
    assert_eq!(mv(&s).picker.unwrap().tab, PickerTab::Favorites);
    // An empty tab adds nothing.
    let (s, cmds) = reduce(&s, Intent::Select);
    assert!(cmds.is_empty());
    let (s, _) = reduce(&s, Intent::Back);
    assert!(mv(&s).picker.is_none());
    assert_eq!(s.screen, Screen::Multiview, "B closes only the picker");
}

#[test]
fn a_on_an_empty_slot_opens_the_picker_for_it() {
    let s = with_streams(3);
    // Grid of 3 holds 3: grow to 4 with the layout, then A on the empty place.
    let (s, _) = reduce(&s, Intent::PageRight);
    assert_eq!(mv(&s).layout, Layout::Grid(4));
    let s = {
        let mut s = s;
        s.multiview.as_mut().unwrap().focus = 3;
        s
    };
    let (s, _) = reduce(&s, Intent::Select);
    assert_eq!(mv(&s).picker.unwrap().place, Some(3));
}

// --- The grid ------------------------------------------------------------------------------------

#[test]
fn the_d_pad_moves_between_places() {
    let s = with_streams(4);
    let mut focus = Vec::new();
    let mut s = s;
    for i in [
        Intent::Right,
        Intent::Down,
        Intent::Left,
        Intent::Up,
        Intent::Up,
    ] {
        s = reduce(&s, i).0;
        focus.push(mv(&s).focus);
    }
    assert_eq!(focus, [1, 3, 2, 0, 0], "stops at the edge");
}

#[test]
fn a_on_a_muted_stream_moves_the_sound_without_reloading() {
    let s = with_streams(3);
    let (s, cmds) = press(&s, &[Intent::Right, Intent::Select]);
    assert_eq!(mv(&s).sound, 1);
    assert_eq!(s.now_playing.as_ref().unwrap().channel.login, "a");
    // Mute the old one, unmute the new one, and only then the swap.
    let at = |c: &Command| cmds.iter().position(|x| x == c).expect("sent");
    let mute = at(&Command::SetMuted {
        slot: 0,
        muted: true,
    });
    let unmute = at(&Command::SetMuted {
        slot: 1,
        muted: false,
    });
    let join = at(&Command::JoinChat {
        channel: ChannelRef::twitch("a"),
    });
    assert!(mute < unmute && unmute < join, "{cmds:?}");
    assert!(cmds.contains(&Command::StopRaids));
    assert!(cmds.contains(&Command::WatchRaids {
        broadcaster_id: "id_a".into()
    }));
    assert!(cmds.contains(&Command::WatchHypeTrain {
        channel: "a".into()
    }));
    assert!(cmds.contains(&Command::FetchStreamInfo {
        channel: ChannelRef::twitch("a")
    }));
    assert!(plays(&cmds).is_empty(), "moving the sound never reloads");
    assert!(settle_id(&cmds).is_none(), "a grid's sizes do not change");
    // Chat starts over.
    assert!(s.chat.is_empty());
    assert_eq!(s.chat_status, ChatStatus::Connecting);
    assert_eq!(s.playback, Playback::Playing);
}

#[test]
fn chat_follows_the_sound_and_y_shows_it_beside_the_grid() {
    let s = with_streams(2);
    let (s, _) = reduce(&s, Intent::ToggleChat);
    assert!(s.chat_open);
    assert_eq!(s.screen, Screen::Multiview);
    // Lines of the stream with sound arrive on the grid; the muted one's do not.
    let line = |login: &str| Event::ChatMessages {
        channel: ChannelRef::twitch(login),
        messages: vec![ChatMessage::plain("x", "hi")],
    };
    let s = reduce(&s, line("main")).0;
    assert_eq!(s.chat.len(), 1);
    assert_eq!(reduce(&s, line("a")).0.chat.len(), 1);
    let (s, _) = reduce(&s, Intent::ToggleChat);
    assert!(!s.chat_open);
}

#[test]
fn lb_and_rb_step_through_the_layouts_that_fit() {
    let mut s = with_streams(3);
    assert_eq!(mv(&s).layout, Layout::Grid(3));
    let mut seen = Vec::new();
    for _ in 0..5 {
        s = reduce(&s, Intent::PageRight).0;
        seen.push(mv(&s).layout);
    }
    // The grid of 2 is too small for 3.
    assert_eq!(
        seen,
        [
            Layout::Grid(4),
            Layout::Grid(6),
            Layout::Grid(9),
            Layout::Spotlight,
            Layout::Grid(3)
        ]
    );
    let (s, _) = reduce(&s, Intent::PageLeft);
    assert_eq!(mv(&s).layout, Layout::Spotlight);
    assert_eq!(mv(&s).tiles.len(), 4, "Spotlight up to 4 has 4 places");
    assert_eq!(mv(&s).count(), 3);
}

#[test]
fn the_grid_popup_offers_add_remove_layout_and_leave() {
    let s = with_streams(2);
    let (s, _) = reduce(&s, Intent::View);
    let popup = s.popup.clone().unwrap();
    assert_eq!(
        popup.rows,
        [
            PopupRow::AddStream,
            PopupRow::Remove,
            PopupRow::Favorite,
            PopupRow::Layout,
            PopupRow::Leave
        ]
    );
    assert_eq!(popup.place, Some(0));
    // Left and Right on the layout row change the layout and keep the popup.
    let (s, _) = press(
        &s,
        &[Intent::Down, Intent::Down, Intent::Down, Intent::Right],
    );
    assert_eq!(mv(&s).layout, Layout::Grid(3));
    assert!(s.popup.is_some());
    // Leave.
    let (s, cmds) = press(&s, &[Intent::Down, Intent::Select]);
    assert!(s.multiview.is_none());
    assert_eq!(s.screen, Screen::Home);
    assert!(cmds.contains(&Command::StopPlayback { slot: 1 }));
}

// --- Removing ------------------------------------------------------------------------------------

#[test]
fn removing_leaves_an_empty_slot() {
    let s = with_streams(4);
    let (s, _) = press(&s, &[Intent::Right, Intent::View]);
    let rows = s.popup.as_ref().unwrap().rows.clone();
    let remove = rows.iter().position(|r| *r == PopupRow::Remove).unwrap();
    let mut s = s;
    s.popup.as_mut().unwrap().focus = remove;
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(
        places(&s),
        [
            Some("main".into()),
            None,
            Some("b".into()),
            Some("c".into())
        ]
    );
    assert_eq!(mv(&s).layout, Layout::Grid(4));
    assert!(matches!(
        &cmds[..],
        [Command::StopPlayback { slot: 1 }, Command::SaveMultiview(_)]
    ));

    // The next stream goes into the gap.
    let (s, _) = reduce(&s, Intent::Select);
    assert_eq!(mv(&s).picker.unwrap().place, Some(1));
    let (s, _) = press(
        &s,
        &[Intent::Down, Intent::Down, Intent::Down, Intent::Select],
    );
    assert_eq!(places(&s)[1], Some("d".into()));
}

#[test]
fn removing_the_stream_with_sound_gives_it_to_the_first_one_left() {
    let s = with_streams(3);
    let (s, _) = reduce(&s, Intent::View);
    let mut s = s;
    let popup = s.popup.as_mut().unwrap();
    popup.focus = popup
        .rows
        .iter()
        .position(|r| *r == PopupRow::Remove)
        .unwrap();
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(places(&s)[0], None);
    assert_eq!(mv(&s).sound, 1);
    assert_eq!(s.now_playing.as_ref().unwrap().channel.login, "a");
    assert!(cmds.contains(&Command::StopPlayback { slot: 0 }));
    assert!(cmds.contains(&Command::SetMuted {
        slot: 1,
        muted: false
    }));
}

#[test]
fn removing_the_last_stream_leaves_multiview() {
    let (s, _) = started();
    let (s, _) = reduce(&s, Intent::Back); // the picker
    let (s, _) = reduce(&s, Intent::View);
    let mut s = s;
    let popup = s.popup.as_mut().unwrap();
    popup.focus = popup
        .rows
        .iter()
        .position(|r| *r == PopupRow::Remove)
        .unwrap();
    let (s, cmds) = reduce(&s, Intent::Select);
    assert!(s.multiview.is_none());
    assert_eq!(s.screen, Screen::Home);
    assert!(cmds.contains(&Command::LeaveChat));
}

// --- Full screen ---------------------------------------------------------------------------------

#[test]
fn a_on_the_stream_with_sound_goes_full_screen_and_b_comes_back() {
    let s = with_streams(2);
    let (full, cmds) = reduce(&s, Intent::Select);
    assert_eq!(full.screen, Screen::Player);
    assert!(full.multiview.is_some());
    // It gets the whole screen once the layout has settled.
    let id = settle_id(&cmds).expect("a settle timer");
    let (full, cmds) = reduce(&full, Event::QualitySettleDue(id));
    assert_eq!(
        cmds,
        [tile_play(
            MAIN_SLOT,
            "main",
            Rendition::P720,
            FrameRate::Smooth
        )]
    );
    let full = reduce(&full, Event::PlaybackStarted { slot: MAIN_SLOT }).0;

    let (grid, cmds) = reduce(&full, Intent::Back);
    assert_eq!(grid.screen, Screen::Multiview);
    assert!(
        plays(&cmds).is_empty(),
        "not paused: nothing reloads at once"
    );
    let id = settle_id(&cmds).expect("back to the tile's size, later");
    let (_, cmds) = reduce(&grid, Event::QualitySettleDue(id));
    assert_eq!(
        cmds,
        [tile_play(
            MAIN_SLOT,
            "main",
            Rendition::P360,
            FrameRate::Light
        )]
    );
}

#[test]
fn in_full_screen_a_pauses_and_play_jumps_back_to_live() {
    let s = with_streams(2);
    let (s, _) = reduce(&s, Intent::Select);
    let (s, cmds) = reduce(&s, Intent::Select);
    assert!(s.paused);
    assert_eq!(
        cmds,
        [Command::SetPaused {
            slot: MAIN_SLOT,
            paused: true
        }]
    );
    let (s, cmds) = reduce(&s, Intent::Select);
    assert!(!s.paused);
    // The paused picture stays until live's first frame replaces it.
    assert_eq!(s.playback, Playback::Playing);
    assert_eq!(plays(&cmds), [MAIN_SLOT], "Play again, from live");
}

#[test]
fn any_button_in_the_grid_brings_its_controls_back() {
    let s = with_streams(3);
    for intent in [Intent::Right, Intent::PageRight, Intent::View] {
        let (after, _) = reduce(&s, intent);
        assert_ne!(after.hud_pulse, s.hud_pulse, "{intent:?}");
    }
    // In the View popup too.
    let (popup, _) = reduce(&s, Intent::View);
    let (after, _) = reduce(&popup, Intent::Down);
    assert_ne!(after.hud_pulse, popup.hud_pulse);
}

#[test]
fn a_quality_reload_keeps_a_playing_tile_playing() {
    let s = with_streams(2);
    let (full, cmds) = reduce(&s, Intent::Select);
    let id = settle_id(&cmds).expect("a settle timer");
    let (full, cmds) = reduce(&full, Event::QualitySettleDue(id));
    assert_eq!(plays(&cmds), [MAIN_SLOT], "full screen gets more");
    // The old quality stays on screen until the new one's first frame.
    assert_eq!(full.playback, Playback::Playing);
    assert_eq!(mv(&full).tile(0).unwrap().state, Playback::Playing);
    assert_eq!(mv(&full).tile(0).unwrap().rendition, Rendition::P720);
}

#[test]
fn b_from_a_paused_full_screen_plays_again() {
    let s = with_streams(2);
    let (s, _) = press(&s, &[Intent::Select, Intent::Select]);
    assert!(s.paused);
    let (s, cmds) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Multiview);
    assert!(!s.paused);
    assert_eq!(
        cmds[0],
        tile_play(MAIN_SLOT, "main", Rendition::P360, FrameRate::Light)
    );
}

#[test]
fn a_tile_in_the_grid_is_never_paused() {
    let s = with_streams(2);
    let (_, cmds) = press(&s, &[Intent::Right, Intent::Select, Intent::Select]);
    assert!(!cmds.iter().any(|c| matches!(c, Command::SetPaused { .. })));
}

#[test]
fn lb_and_rb_in_full_screen_move_the_sound_to_the_next_stream() {
    let s = with_streams(3);
    let (s, _) = reduce(&s, Intent::Select);
    let (s, cmds) = reduce(&s, Intent::PageRight);
    assert_eq!(s.screen, Screen::Player, "still full screen");
    assert_eq!(mv(&s).sound, 1);
    assert_eq!(s.now_playing.as_ref().unwrap().channel.login, "a");
    assert!(cmds.contains(&Command::SetMuted {
        slot: 0,
        muted: true
    }));
    // Wraps around.
    let (s, _) = press(&s, &[Intent::PageRight, Intent::PageRight]);
    assert_eq!(mv(&s).sound, 0);
    let (s, _) = reduce(&s, Intent::PageLeft);
    assert_eq!(mv(&s).sound, 2);
}

#[test]
fn flipping_through_streams_reloads_only_the_one_settled_on() {
    let s = with_streams(3);
    let (s, _) = reduce(&s, Intent::Select);
    let (s, first) = reduce(&s, Intent::PageRight);
    let (s, second) = reduce(&s, Intent::PageRight);
    let (old, new) = (settle_id(&first).unwrap(), settle_id(&second).unwrap());
    assert!(
        reduce(&s, Event::QualitySettleDue(old)).1.is_empty(),
        "replaced"
    );
    let (_, cmds) = reduce(&s, Event::QualitySettleDue(new));
    assert_eq!(
        cmds,
        [tile_play(2, "b", Rendition::P720, FrameRate::Smooth)],
        "main and a never left their tile size for long"
    );
}

#[test]
fn moving_back_before_the_settle_reloads_nothing() {
    let s = with_streams(3);
    // Grid of 3 back to Spotlight, where the stream with sound is bigger, and back again.
    let (s, cmds) = reduce(&s, Intent::PageLeft);
    assert_eq!(mv(&s).layout, Layout::Spotlight);
    let id = settle_id(&cmds).unwrap();
    let (s, cmds) = reduce(&s, Intent::PageRight);
    assert!(settle_id(&cmds).is_none(), "the same sizes as before");
    assert!(reduce(&s, Event::QualitySettleDue(id)).1.is_empty());
}

#[test]
fn grids_with_the_same_tile_size_reload_nothing() {
    // The grids of 3 and 4 have the same tiles.
    let (_, cmds) = reduce(&with_streams(3), Intent::PageRight);
    // Only the new layout is saved (for the resume card).
    assert!(matches!(&cmds[..], [Command::SaveMultiview(_)]));
}

#[test]
fn the_full_screen_popup_offers_add_remove_and_leave() {
    let s = with_streams(2);
    let (s, _) = press(&s, &[Intent::Select, Intent::View]);
    assert_eq!(
        s.popup.as_ref().unwrap().rows,
        [
            PopupRow::AddStream,
            PopupRow::Remove,
            PopupRow::Favorite,
            PopupRow::Leave
        ]
    );
    // Add a stream: back to the grid, with the picker.
    let (s, _) = reduce(&s, Intent::Select);
    assert_eq!(s.screen, Screen::Multiview);
    assert!(mv(&s).picker.is_some());
}

#[test]
fn start_y_and_x_work_in_full_screen_as_on_the_single_player() {
    let s = with_streams(2);
    let (s, _) = reduce(&s, Intent::Select);
    let (menu, _) = reduce(&s, Intent::Menu);
    assert!(menu.player_menu.is_some());
    let (chat, _) = reduce(&s, Intent::ToggleChat);
    assert!(chat.chat_open);
    let (compose, _) = reduce(&chat, Intent::Retry);
    assert!(compose.compose.open);
}

#[test]
fn the_player_menus_stats_follow_the_sound() {
    let s = with_streams(2);
    let (s, _) = press(&s, &[Intent::Right, Intent::Select, Intent::Select]);
    assert_eq!(s.sound_slot(), 1);
    // Start, down to Stats, A.
    let (_, cmds) = press(
        &s,
        &[
            Intent::Menu,
            Intent::Down,
            Intent::Down,
            Intent::Down,
            Intent::Down,
            Intent::Select,
        ],
    );
    assert!(cmds.contains(&Command::SetStats { slot: 1, on: true }));
}

// --- Player reports ------------------------------------------------------------------------------

#[test]
fn player_reports_go_to_the_tile_on_their_slot() {
    let (s, _) = started();
    let (s, _) = reduce(&s, Intent::Select);
    assert_eq!(mv(&s).tile(1).unwrap().state, Playback::Loading);
    let s = reduce(&s, Event::PlaybackStarted { slot: 1 }).0;
    assert_eq!(mv(&s).tile(1).unwrap().state, Playback::Playing);
    let s = reduce(&s, Event::PlaybackEnded { slot: 1 }).0;
    assert_eq!(mv(&s).tile(1).unwrap().state, Playback::Offline);
    assert_eq!(s.playback, Playback::Playing, "the sound's stream is fine");
    // A slot no tile plays on.
    assert_eq!(reduce(&s, Event::PlaybackEnded { slot: 7 }).0, s);
}

#[test]
fn without_multiview_other_slots_are_ignored() {
    let (s, _) = press(&home(true), &[Intent::Select]);
    let s = reduce(&s, Event::PlaybackStarted { slot: 3 }).0;
    assert_eq!(s.playback, Playback::Loading);
}

#[test]
fn an_offline_or_failed_tile_offers_retry_or_remove() {
    let (s, _) = started();
    let (s, _) = reduce(&s, Intent::Select);
    let s = reduce(
        &s,
        Event::StreamOffline {
            channel: ChannelRef::twitch("a"),
        },
    )
    .0;
    assert_eq!(mv(&s).tile(1).unwrap().state, Playback::Offline);
    let (s, _) = reduce(&s, Intent::Back); // the picker
    let (s, _) = reduce(&s, Intent::Select);
    let popup = s.popup.clone().unwrap();
    assert_eq!(popup.rows, [PopupRow::Retry, PopupRow::Remove]);
    assert_eq!(popup.place, Some(1));
    let (retried, cmds) = reduce(&s, Intent::Select);
    assert_eq!(cmds, [tile_play(1, "a", Rendition::P360, FrameRate::Light)]);
    assert_eq!(mv(&retried).tile(1).unwrap().state, Playback::Loading);

    // A failure while loading, then Remove.
    let s = reduce(
        &retried,
        Event::PlaybackFailed {
            channel: ChannelRef::twitch("a"),
            message: "timeout".into(),
        },
    )
    .0;
    assert_eq!(
        mv(&s).tile(1).unwrap().state,
        Playback::Failed("timeout".into())
    );
    let (s, cmds) = press(&s, &[Intent::Select, Intent::Down, Intent::Select]);
    assert_eq!(places(&s), [Some("main".into()), None]);
    assert_eq!(cmds[0], Command::StopPlayback { slot: 1 });
    assert!(plays(&cmds).is_empty());
}

#[test]
fn x_retries_a_failed_tile_in_the_grid() {
    let s = with_streams(2);
    let s = reduce(
        &s,
        Event::PlaybackError {
            slot: 1,
            message: "bad".into(),
        },
    )
    .0;
    let (_, cmds) = press(&s, &[Intent::Right, Intent::Retry]);
    assert_eq!(plays(&cmds), [1]);
}

#[test]
fn the_stream_with_sound_mirrors_its_tile() {
    let s = with_streams(2);
    let s = reduce(
        &s,
        Event::PlaybackError {
            slot: MAIN_SLOT,
            message: "bad".into(),
        },
    )
    .0;
    assert_eq!(s.playback, Playback::Failed("bad".into()));
    // In full screen, A retries it.
    let mut s = s;
    s.screen = Screen::Player;
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(plays(&cmds), [MAIN_SLOT]);
    assert_eq!(s.playback, Playback::Loading);
}

// --- Raids and the stream info -------------------------------------------------------------------

#[test]
fn a_raid_on_the_stream_with_sound_replaces_its_tile_in_place() {
    let s = with_streams(2);
    let (s, _) = reduce(
        &s,
        Event::Raided {
            from_login: "main".into(),
            to_login: "next_up".into(),
            to_name: "Next_Up".into(),
            viewers: 50,
        },
    );
    let id = s.raid.as_ref().expect("the card, on the grid too").id;
    // The card takes the buttons: A does not go full screen.
    let (s, cmds) = reduce(&s, Event::RaidCountdownDone { id });
    assert_eq!(places(&s), [Some("next_up".into()), Some("a".into())]);
    assert_eq!(slot_at(&s, 0), MAIN_SLOT, "same player");
    assert_eq!(s.screen, Screen::Multiview);
    assert_eq!(s.now_playing.as_ref().unwrap().channel.login, "next_up");
    assert!(cmds.contains(&tile_play(
        MAIN_SLOT,
        "next_up",
        Rendition::P360,
        FrameRate::Light
    )));
    assert!(cmds.contains(&Command::JoinChat {
        channel: ChannelRef::twitch("next_up")
    }));
}

#[test]
fn a_raid_from_a_muted_stream_is_ignored() {
    let s = with_streams(2);
    let (s, cmds) = reduce(
        &s,
        Event::Raided {
            from_login: "a".into(),
            to_login: "next_up".into(),
            to_name: String::new(),
            viewers: 50,
        },
    );
    assert!(s.raid.is_none());
    assert!(cmds.is_empty());
}

#[test]
fn the_raid_card_takes_the_grids_buttons() {
    let s = with_streams(2);
    let (s, _) = reduce(
        &s,
        Event::Raided {
            from_login: "main".into(),
            to_login: "next_up".into(),
            to_name: String::new(),
            viewers: 50,
        },
    );
    let (s, cmds) = reduce(&s, Intent::Back);
    assert!(s.raid.is_none(), "B stays on this channel");
    assert!(s.multiview.is_some(), "and does not leave multiview");
    assert!(cmds.is_empty());
}

#[test]
fn stream_info_fills_in_the_tile_too() {
    let s = with_streams(2);
    let info = Stream {
        title: "fresh title".into(),
        ..live("main")
    };
    let s = reduce(&s, Event::StreamInfo(info)).0;
    assert_eq!(mv(&s).tile(0).unwrap().stream.title, "fresh title");
    assert_eq!(s.now_playing.as_ref().unwrap().title, "fresh title");
}

// --- Quality -------------------------------------------------------------------------------------

#[test]
fn a_bigger_screen_gets_more_after_the_settle() {
    let s = with_streams(2);
    let (s, cmds) = reduce(&s, Event::DisplaySize { w: 3840, h: 2160 });
    let id = settle_id(&cmds).expect("every tile changed size");
    let (_, cmds) = reduce(&s, Event::QualitySettleDue(id));
    // A grid of 2 on a 4K TV: 1080p (plan.md, section 3).
    assert_eq!(
        cmds,
        [
            tile_play(0, "main", Rendition::P1080, FrameRate::Light),
            tile_play(1, "a", Rendition::P1080, FrameRate::Light),
        ]
    );
}

#[test]
fn the_quality_setting_caps_every_tile() {
    let mut s = with_streams(2);
    s.settings.quality = Quality::Worst;
    let (s, _) = reduce(&s, Event::DisplaySize { w: 3840, h: 2160 });
    let (_, cmds) = reduce(&s, Event::QualitySettleDue(s.last_settle_id));
    assert_eq!(
        cmds,
        [
            tile_play(0, "main", Rendition::P160, FrameRate::Light),
            tile_play(1, "a", Rendition::P160, FrameRate::Light),
        ]
    );
}

#[test]
fn spotlight_gives_the_stream_with_sound_more() {
    let s = with_streams(3);
    // Grid of 3 -> 4 -> 6 -> 9 -> Spotlight.
    let (s, cmds) = press(
        &s,
        &[
            Intent::PageRight,
            Intent::PageRight,
            Intent::PageRight,
            Intent::PageRight,
        ],
    );
    assert_eq!(mv(&s).layout, Layout::Spotlight);
    let id = settle_id(&cmds).expect("the big tile wants more");
    let (_, cmds) = reduce(&s, Event::QualitySettleDue(id));
    // On a Deck the big tile is 480p and the small ones keep 360p.
    assert_eq!(
        cmds,
        [tile_play(
            MAIN_SLOT,
            "main",
            Rendition::P480,
            FrameRate::Light
        )]
    );
}

#[test]
fn background_focus_loss_never_pauses_multiview() {
    let mut s = with_streams(2);
    s.settings.background_play = false;
    let (s, cmds) = reduce(&s, Event::WindowFocus(false));
    assert!(!s.paused);
    assert!(cmds.is_empty());
}

#[test]
fn the_sleep_timer_leaves_multiview() {
    // Full screen, Start, the sleep timer row, A: 15 minutes.
    let (mut s, _) = press(&with_streams(2), &[Intent::Select, Intent::Menu]);
    s.player_menu = Some(PlayerMenuRow::SleepTimer);
    let (s, _) = reduce(&s, Intent::Select);
    let id = s.sleep.unwrap().id;
    let (s, cmds) = reduce(&s, Event::SleepTimerFired { id });
    assert!(s.multiview.is_none());
    assert_eq!(s.screen, Screen::Home);
    assert!(cmds.contains(&Command::StopPlayback { slot: 1 }));
    assert!(cmds.contains(&Command::Suspend));
}
