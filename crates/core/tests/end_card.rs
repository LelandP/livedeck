//! The live end card, raids at the end of a stream, and waking up (vod-plan.md, phase 7): the
//! card instead of the offline screen, its rows as they become known, the raid card first and a
//! turned-down raid as the card's first row (both orders), the quiet recheck behind the card,
//! the broadcast's video from the card and from the *You were away* toast, and `DeviceWoke` from
//! the wall-clock gap between health reports (`WakeCheck`), live and in a video.

use htpc_core::*;

/// When the broadcast started (Unix seconds).
const STARTED: u64 = 1_794_300_000;
/// When the viewer joined it: ten minutes in.
const JOINED: u64 = STARTED + 600;

fn stream(channel: &str) -> Stream {
    Stream {
        channel: ChannelRef::twitch(channel),
        user_id: "100".into(),
        title: format!("{channel} title"),
        category: "Cat".into(),
        viewers: 1_000,
        tint: Rgb::default(),
        thumbnail: None,
    }
}

fn home_with(tiles: Vec<Tile>) -> AppState {
    let (s, _) = init();
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(RowKind::TopLive, tiles)],
        },
    )
    .0;
    reduce(
        &s,
        Event::SignedIn(User {
            login: "viewer".into(),
            can_chat: true,
        }),
    )
    .0
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

fn has(cmds: &[Command], pred: impl Fn(&Command) -> bool) -> bool {
    cmds.iter().any(pred)
}

/// Watching `ender` (Twitch), playing; its broadcast's video not looked up yet.
fn playing() -> (AppState, Vec<Command>) {
    let s = home_with(vec![Tile::Stream(stream("ender"))]);
    let (s, _) = reduce(&s, Intent::Select);
    reduce(&s, Event::PlaybackStarted { slot: 0 })
}

fn archive_of(channel: &str, id: &str, started_at: u64, checked_at: u64) -> Event {
    Event::LiveArchive {
        channel: ChannelRef::twitch(channel),
        archive: Some(LiveArchive {
            video_id: id.into(),
            started_at,
            checked_at,
        }),
    }
}

/// Playing, with the broadcast's video (`500`) known: joined ten minutes in.
fn watching() -> AppState {
    let (s, _) = playing();
    reduce(&s, archive_of("ender", "500", STARTED, JOINED)).0
}

fn raid() -> Event {
    Event::Raided {
        from_login: "ender".into(),
        to_login: "next_up".into(),
        to_name: "Next Up".into(),
        viewers: 50,
    }
}

fn ended(s: &AppState) -> AppState {
    reduce(s, Event::PlaybackEnded { slot: 0 }).0
}

fn play_video(cmds: &[Command]) -> Option<(String, u64)> {
    cmds.iter().find_map(|c| match c {
        Command::PlayVideo {
            target: VideoTarget::Video(id),
            start_ms,
            ..
        } => Some((id.clone(), *start_ms)),
        _ => None,
    })
}

fn health(pos_ms: Option<u64>) -> Event {
    Event::PlayerHealth {
        slot: MAIN_SLOT,
        at_ms: 0,
        health: Health {
            stalled: false,
            buffer_ms: 5_000,
            pos_ms,
            length_ms: pos_ms.map(|_| 15_128_000),
        },
    }
}

/// The store's loop, with its wake check, at wall-clock times (ms).
struct Store {
    state: AppState,
    wake: WakeCheck,
    cmds: Vec<Command>,
}

impl Store {
    fn new(state: AppState) -> Self {
        Self {
            state,
            wake: WakeCheck::default(),
            cmds: Vec::new(),
        }
    }

    fn at(&mut self, wall_ms: u64, msg: impl Into<Msg>) {
        let msg = msg.into();
        if let Some(woke) = self.wake.before(&msg, wall_ms) {
            self.reduce(woke.into());
        }
        self.reduce(msg);
    }

    fn reduce(&mut self, msg: Msg) {
        let (next, cmds) = reduce(&self.state, msg);
        self.wake.after(&cmds);
        self.state = next;
        self.cmds.extend(cmds);
    }

    fn woke(&self) -> bool {
        // The only trace a wake leaves on a live stream is the broadcast looked up again.
        self.state
            .live_broadcast()
            .is_some_and(|b| b.left_off_at.is_some())
            || self
                .state
                .video
                .as_ref()
                .is_some_and(|v| v.wake_ms.is_some())
    }
}

/// Wall-clock ms of `JOINED + secs`.
fn wall(secs: u64) -> u64 {
    (JOINED + secs) * 1_000
}

// ---- the broadcast ----------------------------------------------------------------------------

#[test]
fn a_twitch_stream_looks_up_its_broadcast_when_it_starts() {
    let (s, cmds) = playing();
    assert!(cmds.contains(&Command::LoadLiveArchive {
        channel: ChannelRef::twitch("ender")
    }));
    let s = reduce(&s, archive_of("ender", "500", STARTED, JOINED)).0;
    let b = s.live_broadcast().expect("known");
    assert_eq!(b.joined_at, JOINED);
    assert_eq!(b.joined_s(), Some(600 - LIVE_BEHIND_S as u32));
    // A later lookup of the same broadcast keeps when the viewer joined; none at all keeps it.
    let s = reduce(&s, archive_of("ender", "500", STARTED, JOINED + 900)).0;
    let s = reduce(
        &s,
        Event::LiveArchive {
            channel: ChannelRef::twitch("ender"),
            archive: None,
        },
    )
    .0;
    let b = s.live_broadcast().unwrap();
    assert_eq!(b.joined_at, JOINED);
    assert_eq!(b.archive.as_ref().unwrap().video_id, "500");
    // Another channel's answer is dropped.
    let after = reduce(&s, archive_of("someone", "9", STARTED, JOINED)).0;
    assert_eq!(after, s);
}

#[test]
fn kick_does_not_look_it_up_and_keeps_the_offline_screen() {
    let kick = Tile::Stream(Stream {
        channel: ChannelRef::kick("kicker"),
        ..stream("kicker")
    });
    let (s, _) = reduce(&home_with(vec![kick]), Intent::Select);
    let (s, cmds) = reduce(&s, Event::PlaybackStarted { slot: 0 });
    assert!(!has(&cmds, |c| matches!(
        c,
        Command::LoadLiveArchive { .. }
    )));
    let s = ended(&s);
    assert_eq!(s.playback, Playback::Offline);
    assert_eq!(s.end_card, None);
    // A still tries again there.
    let (_, cmds) = reduce(&s, Intent::Select);
    assert!(has(&cmds, |c| matches!(c, Command::Play { .. })));
}

// ---- the end card -----------------------------------------------------------------------------

#[test]
fn the_end_card_replaces_the_offline_screen_with_the_broadcast() {
    let s = ended(&watching());
    assert_eq!(s.playback, Playback::Offline);
    assert_eq!(
        s.live_end_rows(),
        [
            LiveEndRow::Joined,
            LiveEndRow::FromStart,
            LiveEndRow::ChannelVideos,
            LiveEndRow::Back
        ]
    );
    assert_eq!(s.end_card, Some(LiveEndRow::Joined));
    // The broadcast's video not known: only the channel page and Back.
    let (s2, _) = playing();
    let s2 = ended(&s2);
    assert_eq!(
        s2.live_end_rows(),
        [LiveEndRow::ChannelVideos, LiveEndRow::Back]
    );
    assert_eq!(s2.end_card, Some(LiveEndRow::ChannelVideos));
}

#[test]
fn up_and_down_move_and_stop_at_the_ends() {
    let s = ended(&watching());
    let (s, _) = run(&s, vec![Intent::Up.into()]);
    assert_eq!(s.end_card, Some(LiveEndRow::Joined));
    let (s, _) = run(&s, vec![Intent::Down.into(); 5]);
    assert_eq!(s.end_card, Some(LiveEndRow::Back));
    let (s, _) = run(&s, vec![Intent::Up.into()]);
    assert_eq!(s.end_card, Some(LiveEndRow::ChannelVideos));
}

#[test]
fn the_broadcast_plays_from_where_the_viewer_joined_over_the_parked_stream() {
    let s = ended(&watching());
    let (v, cmds) = reduce(&s, Intent::Select);
    assert_eq!(
        play_video(&cmds),
        Some(("500".into(), (600 - LIVE_BEHIND_S) * 1_000))
    );
    assert!(cmds.contains(&Command::StopPlayback { slot: 0 }));
    assert!(cmds.contains(&Command::LeaveChat));
    assert_eq!(v.screen, Screen::Player);
    assert_eq!(v.now_playing, None);
    assert_eq!(v.parked_live.as_ref().unwrap().channel.login, "ender");
    assert_eq!(v.end_card, None);
    let video = v.video.as_ref().unwrap();
    assert_eq!(video.item.channel().login, "ender");
    assert_eq!(video.item.title(), "ender title");

    // B: back to the live player, which looks for the stream again.
    let (back, cmds) = reduce(&v, Intent::Back);
    assert_eq!(back.screen, Screen::Player);
    assert_eq!(back.video, None);
    assert_eq!(back.now_playing.as_ref().unwrap().channel.login, "ender");
    assert_eq!(back.playback, Playback::Loading);
    assert!(has(&cmds, |c| matches!(c, Command::Play { .. })));
    // Still offline: the same end card, the broadcast still known.
    let (back, _) = reduce(
        &back,
        Event::StreamOffline {
            channel: ChannelRef::twitch("ender"),
        },
    );
    assert_eq!(back.end_card, Some(LiveEndRow::Joined));
    assert_eq!(back.live_end_rows().len(), 4);
    // And B there leaves the player for Home.
    let (home, _) = reduce(&back, Intent::Back);
    assert_eq!(home.screen, Screen::Home);
}

#[test]
fn from_the_start_plays_at_zero() {
    let s = ended(&watching());
    let (_, cmds) = run(&s, vec![Intent::Down.into(), Intent::Select.into()]);
    assert_eq!(play_video(&cmds), Some(("500".into(), 0)));
}

#[test]
fn channel_videos_goes_over_the_card_and_the_recheck_runs_on() {
    let s = ended(&watching());
    let (page, cmds) = run(
        &s,
        vec![
            Intent::Down.into(),
            Intent::Down.into(),
            Intent::Select.into(),
        ],
    );
    assert_eq!(page.screen, Screen::Channel);
    assert_eq!(page.back_stack.last(), Some(&Screen::Player));
    assert!(cmds.contains(&Command::LoadChannelPage {
        channel: ChannelRef::twitch("ender")
    }));
    assert!(page.channel_page.as_ref().unwrap().live.is_none());
    // The quiet recheck still looks for the stream.
    let (_, cmds) = reduce(&page, Event::OfflineRecheckDue(page.offline_recheck));
    assert!(has(&cmds, |c| matches!(c, Command::Play { .. })));
    // B: the end card again, focus where it was.
    let (back, _) = reduce(&page, Intent::Back);
    assert_eq!(back.screen, Screen::Player);
    assert_eq!(back.end_card, Some(LiveEndRow::ChannelVideos));
}

#[test]
fn back_on_the_card_leaves_the_player() {
    let s = ended(&watching());
    let (s, cmds) = run(&s, vec![Intent::Down.into(); 3]);
    assert_eq!(s.end_card, Some(LiveEndRow::Back));
    assert!(cmds.is_empty());
    let (home, cmds) = reduce(&s, Intent::Select);
    assert_eq!(home.screen, Screen::Home);
    assert!(cmds.contains(&Command::StopPlayback { slot: 0 }));
    let (home, _) = reduce(&ended(&watching()), Intent::Back);
    assert_eq!(home.screen, Screen::Home);
}

#[test]
fn the_recheck_brings_the_stream_back_while_the_card_shows() {
    let s = ended(&watching());
    let (s, cmds) = reduce(&s, Event::OfflineRecheckDue(s.offline_recheck));
    assert!(has(&cmds, |c| matches!(c, Command::Play { .. })));
    // A failed look keeps the card where it was.
    let (s, _) = run(&s, vec![Intent::Down.into()]);
    let (s, _) = reduce(
        &s,
        Event::StreamOffline {
            channel: ChannelRef::twitch("ender"),
        },
    );
    assert_eq!(s.end_card, Some(LiveEndRow::FromStart));
    let (s, _) = reduce(&s, Event::OfflineRecheckDue(s.offline_recheck));
    // Back: it plays, the card goes, and the (maybe new) broadcast is looked up.
    let (s, cmds) = reduce(&s, Event::PlaybackStarted { slot: 0 });
    assert_eq!(s.playback, Playback::Playing);
    assert_eq!(s.end_card, None);
    assert!(cmds.contains(&Command::LoadLiveArchive {
        channel: ChannelRef::twitch("ender")
    }));
}

// ---- raids ------------------------------------------------------------------------------------

#[test]
fn offline_then_raid_the_card_goes_over_the_end_card_and_b_makes_it_the_first_row() {
    let s = ended(&watching());
    let (s, cmds) = reduce(&s, raid());
    assert!(s.raid.is_some(), "the raid card first");
    assert!(has(&cmds, |c| matches!(
        c,
        Command::StartRaidCountdown { .. }
    )));
    assert_eq!(s.end_card, Some(LiveEndRow::Joined), "under it");
    // The card takes the buttons: Down does not move the end card.
    let (s, _) = reduce(&s, Intent::Down);
    assert_eq!(s.end_card, Some(LiveEndRow::Joined));
    // B turns it down: the raid leads the card, the focus stays.
    let (s, _) = reduce(&s, Intent::Back);
    assert!(s.raid.is_none());
    assert_eq!(s.screen, Screen::Player);
    assert_eq!(s.live_end_rows()[0], LiveEndRow::FollowRaid);
    assert_eq!(s.end_card, Some(LiveEndRow::Joined));
    // A on it follows the raid.
    let (s, _) = reduce(&s, Intent::Up);
    assert_eq!(s.end_card, Some(LiveEndRow::FollowRaid));
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(s.now_playing.as_ref().unwrap().channel.login, "next_up");
    assert_eq!(s.playback, Playback::Loading);
    assert_eq!(s.end_card, None);
    assert!(has(
        &cmds,
        |c| matches!(c, Command::Play { channel, .. } if channel.login == "next_up")
    ));
    // The new channel's broadcast is its own.
    assert_eq!(s.live_broadcast(), None);
}

#[test]
fn raid_then_offline_the_raid_card_stays_first_and_counts_down() {
    let (s, _) = reduce(&watching(), raid());
    let id = s.raid.as_ref().unwrap().id;
    let s = ended(&s);
    assert!(s.raid.is_some(), "still the raid card first");
    assert_eq!(
        s.end_card,
        Some(LiveEndRow::Joined),
        "the end card after it"
    );
    // Its countdown ending follows the raid.
    let (s, _) = reduce(&s, Event::RaidCountdownDone { id });
    assert_eq!(s.now_playing.as_ref().unwrap().channel.login, "next_up");
    assert_eq!(s.end_card, None);
}

#[test]
fn raid_then_offline_turned_down_becomes_the_first_row() {
    let (s, _) = reduce(&watching(), raid());
    let s = ended(&s);
    let (s, _) = reduce(&s, Intent::Back);
    assert_eq!(
        s.live_end_rows(),
        [
            LiveEndRow::FollowRaid,
            LiveEndRow::Joined,
            LiveEndRow::FromStart,
            LiveEndRow::ChannelVideos,
            LiveEndRow::Back
        ]
    );
}

#[test]
fn a_raid_turned_down_while_live_leads_the_card_when_the_stream_ends() {
    let (s, _) = reduce(&watching(), raid());
    let (s, _) = reduce(&s, Intent::Back);
    assert_eq!(s.playback, Playback::Playing);
    let s = ended(&s);
    assert_eq!(s.live_end_rows()[0], LiveEndRow::FollowRaid);
    // The focus starts past it, on the broadcast.
    assert_eq!(s.end_card, Some(LiveEndRow::Joined));
}

// ---- waking up --------------------------------------------------------------------------------

#[test]
fn a_gap_between_reports_is_a_wake_and_a_pause_or_a_load_is_not() {
    let mut w = WakeCheck::default();
    let tick = Msg::from(health(None));
    assert_eq!(w.before(&tick, 1_000), None);
    assert_eq!(w.before(&tick, 2_000), None);
    assert_eq!(
        w.before(&tick, 2_000 + 3_600_000),
        Some(Event::DeviceWoke {
            asleep_ms: 3_600_000,
            before_s: 2
        })
    );
    // Exactly at the limit is not one.
    assert_eq!(w.before(&tick, 2_000 + 3_600_000 + WAKE_GAP_MS), None);

    // A pause asked for in between starts over.
    let mut w = WakeCheck::default();
    w.before(&tick, 1_000);
    w.after(&[Command::SetPaused {
        slot: 0,
        paused: true,
    }]);
    assert_eq!(w.before(&tick, 1_000 + 3_600_000), None);
    // So do a load, a stop, the first frame, and the end of the stream.
    for restart in [
        Msg::from(Event::PlaybackStarted { slot: 0 }),
        Msg::from(Event::PlaybackEnded { slot: 0 }),
    ] {
        let mut w = WakeCheck::default();
        w.before(&tick, 1_000);
        w.before(&restart, 2_000);
        assert_eq!(w.before(&tick, 2_000 + 3_600_000), None);
    }
    let mut w = WakeCheck::default();
    w.before(&tick, 1_000);
    w.after(&[Command::StopPlayback { slot: 0 }]);
    assert_eq!(w.before(&tick, 1_000 + 3_600_000), None);
    // Another slot (a multiview tile) is never one.
    let mut w = WakeCheck::default();
    let tile = Msg::from(Event::PlayerHealth {
        slot: 3,
        at_ms: 0,
        health: Health::default(),
    });
    w.before(&tile, 1_000);
    assert_eq!(w.before(&tile, 1_000 + 3_600_000), None);
    // An end or an error after a gap is one (the stream ended in the sleep).
    let mut w = WakeCheck::default();
    w.before(&tick, 1_000);
    assert!(matches!(
        w.before(
            &Msg::from(Event::PlaybackEnded { slot: 0 }),
            1_000 + 600_000
        ),
        Some(Event::DeviceWoke { .. })
    ));
}

#[test]
fn paused_then_a_gap_is_not_a_wake() {
    let mut store = Store::new(watching());
    store.at(wall(0), health(None));
    store.at(wall(1), health(None));
    store.at(wall(2), Intent::Select); // pause
    assert!(store.state.paused);
    // An hour later, play again: the first report after the pause is not a wake.
    store.at(wall(3_600), Intent::Select);
    store.at(wall(3_601), health(None));
    assert!(!store.woke());
    assert!(store.state.toasts.is_empty());
}

#[test]
fn wake_while_live_offers_the_part_slept_through() {
    let mut store = Store::new(watching());
    store.at(wall(0), health(None));
    store.at(wall(1), health(None));
    store.cmds.clear();
    // The device sleeps 2 h 10 min; the stream is still live.
    let away = (2 * 3_600 + 10 * 60) * 1_000;
    store.at(wall(1) + away, health(None));
    assert!(store.woke());
    assert!(store.cmds.contains(&Command::LoadLiveArchive {
        channel: ChannelRef::twitch("ender")
    }));
    assert!(
        store.state.toasts.is_empty(),
        "not before the broadcast is known"
    );
    // The same broadcast: the toast.
    store.at(
        wall(1) + away + 500,
        archive_of("ender", "500", STARTED, JOINED + 7_801),
    );
    let toast = store.state.toasts.first().expect("the toast");
    assert_eq!(toast.kind, ToastKind::Away { away_ms: away });
    // X: the broadcast from the last report before the sleep, less the latency.
    let (v, cmds) = reduce(&store.state, Intent::Retry);
    let left_off = 600 + 1 - LIVE_BEHIND_S;
    assert_eq!(play_video(&cmds), Some(("500".into(), left_off * 1_000)));
    assert!(v.toasts.is_empty());
    assert_eq!(v.parked_live.as_ref().unwrap().channel.login, "ender");
    assert!(v.video.as_ref().is_some_and(|v| matches!(
        &v.item,
        VideoItem::Video(video) if video.recording
    )));
    // B: the live stream again.
    let (back, cmds) = reduce(&v, Intent::Back);
    assert_eq!(back.now_playing.as_ref().unwrap().channel.login, "ender");
    assert!(has(&cmds, |c| matches!(c, Command::Play { .. })));
}

#[test]
fn without_the_toast_x_writes_in_chat_as_before() {
    let mut s = watching();
    s.chat_open = true;
    let (s, cmds) = reduce(&s, Intent::Retry);
    assert!(s.compose.open);
    assert!(play_video(&cmds).is_none());
}

#[test]
fn a_broadcast_restarted_in_the_sleep_gets_no_toast() {
    let mut store = Store::new(watching());
    store.at(wall(0), health(None));
    store.at(wall(3_600), health(None));
    assert!(store.woke());
    // The broadcast now is another one, started after the viewer fell asleep.
    store.at(
        wall(3_601),
        archive_of("ender", "501", JOINED + 1_800, JOINED + 3_601),
    );
    assert!(store.state.toasts.is_empty());
    assert_eq!(store.state.live_broadcast().unwrap().left_off_s(), None);
}

#[test]
fn wake_after_the_stream_ended_in_the_sleep_shows_the_card_with_where_you_left_off() {
    let mut store = Store::new(watching());
    store.at(wall(0), health(None));
    store.at(wall(1_200), health(None));
    // Asleep for an hour; on waking the player says the stream ended.
    store.at(wall(1_200 + 3_600), Event::PlaybackEnded { slot: 0 });
    let s = &store.state;
    assert_eq!(s.playback, Playback::Offline);
    assert_eq!(
        s.live_end_rows(),
        [
            LiveEndRow::LeftOff,
            LiveEndRow::Joined,
            LiveEndRow::FromStart,
            LiveEndRow::ChannelVideos,
            LiveEndRow::Back
        ]
    );
    assert_eq!(s.end_card, Some(LiveEndRow::LeftOff));
    // The lookup after the wake finds no recording any more: the card stays, no toast.
    store.at(
        wall(1_200 + 3_601),
        Event::LiveArchive {
            channel: ChannelRef::twitch("ender"),
            archive: None,
        },
    );
    assert!(store.state.toasts.is_empty());
    assert_eq!(store.state.live_end_rows()[0], LiveEndRow::LeftOff);
    let (_, cmds) = reduce(&store.state, Intent::Select);
    let left_off = 600 + 1_200 - LIVE_BEHIND_S;
    assert_eq!(play_video(&cmds), Some(("500".into(), left_off * 1_000)));
}

#[test]
fn an_error_just_after_a_wake_is_the_end_of_the_stream_not_an_error_screen() {
    let mut store = Store::new(watching());
    store.at(wall(0), health(None));
    store.at(
        wall(3_600),
        Event::PlaybackError {
            slot: 0,
            message: "connection reset".into(),
        },
    );
    assert_eq!(store.state.playback, Playback::Offline);
    assert_eq!(store.state.end_card, Some(LiveEndRow::LeftOff));
    // An error while it plays on is an error, as before.
    let mut store = Store::new(watching());
    store.at(wall(0), health(None));
    store.at(wall(1), health(None));
    store.at(
        wall(2),
        Event::PlaybackError {
            slot: 0,
            message: "boom".into(),
        },
    );
    assert_eq!(store.state.playback, Playback::Failed("boom".into()));
}

// ---- waking up in a video ---------------------------------------------------------------------

/// The broadcast's video playing from the start of the end card, at `pos_ms`.
fn in_a_video(pos_ms: u64) -> Store {
    let s = ended(&watching());
    let (s, _) = run(&s, vec![Intent::Down.into(), Intent::Select.into()]);
    let (s, _) = reduce(&s, Event::PlaybackStarted { slot: 0 });
    let mut store = Store::new(s);
    store.at(wall(10), health(Some(pos_ms - 1_000)));
    store.at(wall(11), health(Some(pos_ms)));
    store.cmds.clear();
    store
}

#[test]
fn wake_in_a_video_that_fails_starts_again_from_before_the_sleep_once() {
    let mut store = in_a_video(4_000_000);
    // Asleep an hour. mpv jumps ahead by itself, then fails: the address expired.
    store.at(wall(11 + 3_600), health(Some(4_020_000)));
    assert_eq!(store.state.video.as_ref().unwrap().wake_ms, Some(4_000_000));
    store.at(
        wall(11 + 3_602),
        Event::PlaybackError {
            slot: 0,
            message: "403".into(),
        },
    );
    assert_eq!(store.state.playback, Playback::Loading);
    assert_eq!(play_video(&store.cmds), Some(("500".into(), 4_000_000)));
    // A second failure shows the error.
    store.at(
        wall(11 + 3_610),
        Event::PlaybackError {
            slot: 0,
            message: "403".into(),
        },
    );
    assert!(matches!(store.state.playback, Playback::Failed(_)));
}

#[test]
fn wake_in_a_video_that_ends_early_starts_again() {
    let mut store = in_a_video(4_000_000);
    store.at(wall(11 + 3_600), Event::PlaybackEnded { slot: 0 });
    assert_eq!(store.state.playback, Playback::Loading);
    assert_eq!(play_video(&store.cmds), Some(("500".into(), 4_000_000)));
    assert_eq!(store.state.video.as_ref().unwrap().end_card, None);
}

#[test]
fn wake_in_a_video_that_plays_on_changes_nothing_and_leaving_saves_the_place_before_it() {
    let mut store = in_a_video(4_000_000);
    store.at(wall(11 + 3_600), health(Some(4_001_000)));
    // Left soon after: the place before the sleep.
    let (left, _) = reduce(&store.state, Intent::Back);
    let saved = left
        .continue_watching
        .iter()
        .find(|p| p.video.id == "500")
        .expect("saved");
    assert_eq!(saved.pos_s, 4_000);
    // Played on a minute past it: mpv carried on, a later failure is an error.
    store.at(wall(11 + 3_661), health(Some(4_061_000)));
    assert_eq!(store.state.video.as_ref().unwrap().wake_ms, None);
    store.at(
        wall(11 + 3_662),
        Event::PlaybackError {
            slot: 0,
            message: "boom".into(),
        },
    );
    assert!(matches!(store.state.playback, Playback::Failed(_)));
}

#[test]
fn a_paused_video_left_for_an_hour_is_not_a_wake() {
    let mut store = in_a_video(4_000_000);
    store.at(wall(12), Intent::Select); // pause
    store.at(wall(12 + 3_600), Intent::Select); // play
    store.at(wall(12 + 3_601), health(Some(4_001_000)));
    assert!(!store.woke());
    store.at(
        wall(12 + 3_602),
        Event::PlaybackError {
            slot: 0,
            message: "boom".into(),
        },
    );
    assert!(matches!(store.state.playback, Playback::Failed(_)));
}

#[test]
fn the_sleep_timer_saves_the_place_from_its_warning() {
    let mut store = in_a_video(4_000_000);
    store.state.sleep = Some(SleepTimer {
        id: 7,
        minutes: 15,
        left_min: 1,
        warning: false,
    });
    store.at(wall(12), Event::SleepTimerWarning { id: 7 });
    assert_eq!(
        store.state.video.as_ref().unwrap().warned_ms,
        Some(4_000_000)
    );
    // The video plays on while the viewer sleeps.
    store.at(wall(13), health(Some(4_002_000)));
    store.at(wall(14), health(Some(4_003_000)));
    assert!(!store.woke());
    store.at(wall(15), Event::SleepTimerFired { id: 7 });
    let s = &store.state;
    let saved = s
        .continue_watching
        .iter()
        .find(|p| p.video.id == "500")
        .expect("saved");
    assert_eq!(saved.pos_s, 4_000);
    assert_eq!(store.cmds.last(), Some(&Command::Suspend));
    // The live stream behind the video is not loaded again.
    assert_eq!(s.screen, Screen::Home);
    assert!(s.now_playing.is_none() && s.parked_live.is_none());
    assert!(!has(&store.cmds, |c| matches!(c, Command::Play { .. })));
}

#[test]
fn a_button_during_the_warning_keeps_watching_and_forgets_its_place() {
    let mut store = in_a_video(4_000_000);
    store.state.sleep = Some(SleepTimer {
        id: 7,
        minutes: 15,
        left_min: 1,
        warning: false,
    });
    store.at(wall(12), Event::SleepTimerWarning { id: 7 });
    store.at(wall(13), Intent::Up);
    assert_eq!(store.state.video.as_ref().unwrap().warned_ms, None);
}
