//! The slow-network rule (plan.md, section 4): `NetLimit` on scripted health reports, then the
//! reducer lowering and raising streams in multiview and in the single player.
//!
//! The display is never reported here, so it counts as the Deck's 1280x800: grid tiles want 360p.

use htpc_core::netlimit::{
    Change, CALM_MAX_MS, CALM_MS, LOW_FOR_MS, RELAPSE_MS, REPORT_GAP_MS, STEP_GAP_MS,
};
use htpc_core::*;
use Tier::{MutedBig, MutedSmall, Sound};

// ---------------------------------------------------------------------------------------------
// NetLimit
// ---------------------------------------------------------------------------------------------

const OK: Health = Health {
    stalled: false,
    buffer_ms: 4_000,
    pos_ms: None,
    length_ms: None,
};
const LOW: Health = Health {
    stalled: false,
    buffer_ms: 600,
    pos_ms: None,
    length_ms: None,
};
const STALL: Health = Health {
    stalled: true,
    buffer_ms: 0,
    pos_ms: None,
    length_ms: None,
};

/// One report: when (ms), from which slot, and how it was.
type Report = (u64, Slot, Health);

/// Feeds `script` to `net` with every tier having room, and returns each change with its time.
fn feed(net: &mut NetLimit, script: &[Report]) -> Vec<(u64, Change)> {
    feed_with_room(net, script, |_| true)
}

fn feed_with_room(
    net: &mut NetLimit,
    script: &[Report],
    room: impl Fn(Tier) -> bool + Copy,
) -> Vec<(u64, Change)> {
    script
        .iter()
        .filter_map(|(at, slot, h)| net.report(*slot, *h, *at, room).map(|c| (*at, c)))
        .collect()
}

/// Like `feed`, but every tier has streams that can go only `steps` renditions lower in all.
fn feed_until(net: &mut NetLimit, script: &[Report], steps: u8) -> Vec<(u64, Change)> {
    script
        .iter()
        .filter_map(|(at, slot, h)| {
            let before = net.clone();
            net.report(*slot, *h, *at, |t| before.steps(t) < steps)
                .map(|c| (*at, c))
        })
        .collect()
}

/// `slot` reporting `health` every second from `from` ms up to and including `to` ms.
fn every_second(slot: Slot, health: Health, from: u64, to: u64) -> Vec<Report> {
    (from..=to)
        .step_by(1_000)
        .map(|at| (at, slot, health))
        .collect()
}

#[test]
fn healthy_reports_change_nothing() {
    let mut net = NetLimit::default();
    assert!(feed(&mut net, &every_second(1, OK, 0, 300_000)).is_empty());
    assert!(!net.is_limited());
}

#[test]
fn a_stall_lowers_muted_small_then_muted_big_then_the_sound() {
    let mut net = NetLimit::default();
    let script = [
        (0, 1, STALL),
        (STEP_GAP_MS, 1, STALL),
        (2 * STEP_GAP_MS, 1, STALL),
        (3 * STEP_GAP_MS, 1, STALL),
    ];
    // Each tier can go one rendition lower: they go in turn, and then nothing is left.
    assert_eq!(
        feed_until(&mut net, &script, 1),
        [
            (0, Change::Lowered(MutedSmall)),
            (STEP_GAP_MS, Change::Lowered(MutedBig)),
            (2 * STEP_GAP_MS, Change::Lowered(Sound)),
        ]
    );
    assert_eq!(
        [net.steps(MutedSmall), net.steps(MutedBig), net.steps(Sound)],
        [1, 1, 1]
    );

    // The muted small tiles go as low as they can before the next tier is touched.
    let mut net = NetLimit::default();
    let lowered: Vec<Change> = feed_until(&mut net, &script, 3)
        .into_iter()
        .map(|(_, c)| c)
        .collect();
    assert_eq!(
        lowered,
        [
            Change::Lowered(MutedSmall),
            Change::Lowered(MutedSmall),
            Change::Lowered(MutedSmall),
            Change::Lowered(MutedBig),
        ]
    );
    assert_eq!(net.steps(Sound), 0);
}

#[test]
fn a_tier_without_room_is_skipped_and_with_none_nothing_moves() {
    // A grid: no muted tile is bigger than another, so there are no muted big ones.
    let mut net = NetLimit::default();
    let grid = |t: Tier| t != MutedBig;
    let script = [(0, 1, STALL), (STEP_GAP_MS, 1, STALL)];
    assert_eq!(
        feed_with_room(&mut net, &script, grid),
        [
            (0, Change::Lowered(MutedSmall)),
            (STEP_GAP_MS, Change::Lowered(MutedSmall)),
        ]
    );
    // The single player: only the sound.
    let mut net = NetLimit::default();
    assert_eq!(
        feed_with_room(&mut net, &[(0, 0, STALL)], |t| t == Sound),
        [(0, Change::Lowered(Sound))]
    );
    // Everything at 160p already.
    let mut net = NetLimit::default();
    assert!(feed_with_room(&mut net, &[(0, 0, STALL)], |_| false).is_empty());
    assert!(!net.is_limited());
}

#[test]
fn steps_are_at_least_ten_seconds_apart() {
    let mut net = NetLimit::default();
    let script = every_second(2, STALL, 0, 25_000);
    let changes = feed(&mut net, &script);
    let times: Vec<u64> = changes.iter().map(|(at, _)| *at).collect();
    assert_eq!(times, [0, STEP_GAP_MS, 2 * STEP_GAP_MS]);
}

#[test]
fn a_low_buffer_struggles_once_it_has_lasted_five_seconds() {
    // Low from 0: at 4 s not yet, at 5 s it is.
    let mut net = NetLimit::default();
    let changes = feed(&mut net, &every_second(1, LOW, 0, LOW_FOR_MS));
    assert_eq!(changes, [(LOW_FOR_MS, Change::Lowered(MutedSmall))]);

    // A healthy report in between starts the spell again.
    let mut net = NetLimit::default();
    let mut script = every_second(1, LOW, 0, 3_000);
    script.push((4_000, 1, OK));
    script.extend(every_second(1, LOW, 5_000, 9_000));
    assert!(feed(&mut net, &script).is_empty());
    assert_eq!(
        feed(&mut net, &[(10_000, 1, LOW)]),
        [(10_000, Change::Lowered(MutedSmall))]
    );
}

#[test]
fn a_low_buffer_spell_is_per_player_and_breaks_on_a_gap() {
    // Two players taking turns being low: neither has been low for 5 s.
    let mut net = NetLimit::default();
    let mut script = Vec::new();
    for k in 0..6u64 {
        let at = k * 1_000;
        script.push((at, 1, if k % 2 == 0 { LOW } else { OK }));
        script.push((at, 2, if k % 2 == 0 { OK } else { LOW }));
    }
    assert!(feed(&mut net, &script).is_empty());

    // Reports further apart than the gap (the tile reloaded) do not join up.
    let mut net = NetLimit::default();
    let gap = REPORT_GAP_MS + 500;
    let script = [
        (0, 3, LOW),
        (gap, 3, LOW),
        (2 * gap, 3, LOW),
        (3 * gap, 3, LOW),
    ];
    assert!(feed(&mut net, &script).is_empty());
}

#[test]
fn two_calm_minutes_raise_one_step_the_sound_first() {
    let mut net = NetLimit::default();
    let stalls = [
        (0, 1, STALL),
        (STEP_GAP_MS, 1, STALL),
        (2 * STEP_GAP_MS, 1, STALL),
    ];
    feed_until(&mut net, &stalls, 1);
    assert!(net.steps(Sound) == 1 && net.steps(MutedBig) == 1 && net.steps(MutedSmall) == 1);

    let start = 2 * STEP_GAP_MS;
    let changes = feed(
        &mut net,
        &every_second(1, OK, start + 1_000, start + 3 * CALM_MS),
    );
    assert_eq!(
        changes,
        [
            (start + CALM_MS, Change::Raised(Sound)),
            (start + 2 * CALM_MS, Change::Raised(MutedBig)),
            (start + 3 * CALM_MS, Change::Raised(MutedSmall)),
        ]
    );
    assert!(!net.is_limited());
}

#[test]
fn a_low_buffer_restarts_the_calm_wait() {
    let mut net = NetLimit::default();
    feed(&mut net, &[(0, 1, STALL)]);
    // Calm for 100 s, one low report, then calm again: the two minutes count from it.
    let mut script = every_second(1, OK, 1_000, 100_000);
    script.push((101_000, 1, LOW));
    script.extend(every_second(1, OK, 102_000, 101_000 + CALM_MS));
    assert_eq!(
        feed(&mut net, &script),
        [(101_000 + CALM_MS, Change::Raised(MutedSmall))]
    );
}

#[test]
fn time_without_reports_is_not_calm() {
    let mut net = NetLimit::default();
    feed(&mut net, &[(0, 1, STALL)]);
    feed(&mut net, &every_second(1, OK, 1_000, 60_000));
    // Nothing was watched for ten minutes: the wait starts again when reports come back.
    let back = 660_000;
    assert!(feed(&mut net, &[(back, 1, OK)]).is_empty());
    assert_eq!(
        feed(&mut net, &every_second(1, OK, back + 1_000, back + CALM_MS)),
        [(back + CALM_MS, Change::Raised(MutedSmall))]
    );
}

#[test]
fn a_stall_soon_after_rising_drops_back_at_once_and_doubles_the_wait() {
    let mut net = NetLimit::default();
    feed(&mut net, &[(0, 0, STALL)]);
    let rose = CALM_MS;
    assert_eq!(
        feed(&mut net, &every_second(0, OK, 1_000, rose)),
        [(rose, Change::Raised(MutedSmall))]
    );
    // 5 s later, inside the 10 s gap: it still drops back at once.
    let stall = rose + 5_000;
    assert_eq!(
        feed(&mut net, &[(stall, 0, STALL)]),
        [(stall, Change::Lowered(MutedSmall))]
    );
    assert_eq!(net.calm_needed_ms(), 2 * CALM_MS);
    // Two minutes are not enough now; four are.
    assert_eq!(
        feed(
            &mut net,
            &every_second(0, OK, stall + 1_000, stall + 2 * CALM_MS)
        ),
        [(stall + 2 * CALM_MS, Change::Raised(MutedSmall))]
    );
}

#[test]
fn the_wait_doubles_up_to_ten_minutes_and_a_rise_that_lasts_resets_it() {
    let mut net = NetLimit::default();
    feed(&mut net, &[(0, 0, STALL)]);
    let mut at = 0;
    let mut waits = Vec::new();
    for _ in 0..4 {
        let wait = net.calm_needed_ms();
        let rose = at + wait;
        feed(&mut net, &every_second(0, OK, at + 1_000, rose));
        at = rose + 1_000;
        feed(&mut net, &[(at, 0, STALL)]);
        waits.push(net.calm_needed_ms());
    }
    assert_eq!(waits, [240_000, 480_000, CALM_MAX_MS, CALM_MAX_MS]);

    // This time the rise lasts past 30 s: the wait is two minutes again.
    let rose = at + net.calm_needed_ms();
    feed(
        &mut net,
        &every_second(0, OK, at + 1_000, rose + RELAPSE_MS),
    );
    assert_eq!(net.calm_needed_ms(), CALM_MS);
}

#[test]
fn a_stall_long_after_rising_is_an_ordinary_step() {
    let mut net = NetLimit::default();
    feed(&mut net, &[(0, 0, STALL)]);
    feed(&mut net, &every_second(0, OK, 1_000, CALM_MS));
    let late = CALM_MS + RELAPSE_MS + 1_000;
    feed(
        &mut net,
        &every_second(0, OK, CALM_MS + 1_000, late - 1_000),
    );
    assert_eq!(
        feed(&mut net, &[(late, 0, STALL)]),
        [(late, Change::Lowered(MutedSmall))]
    );
    assert_eq!(net.calm_needed_ms(), CALM_MS);
}

#[test]
fn a_slot_past_the_last_is_ignored() {
    let mut net = NetLimit::default();
    assert!(feed(&mut net, &[(0, 9, STALL), (0, 200, STALL)]).is_empty());
}

// ---------------------------------------------------------------------------------------------
// The reducer
// ---------------------------------------------------------------------------------------------

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

const FOLLOWED: [&str; 4] = ["a", "b", "c", "d"];

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

/// Home with the live stream `main`, signed in, Following holding `FOLLOWED`.
fn home() -> AppState {
    let (s, _) = init();
    let msgs: Vec<Msg> = vec![
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(RowKind::TopLive, vec![Tile::Stream(live("main"))])],
        }
        .into(),
        Event::SignedIn(User {
            login: "viewer".into(),
            can_chat: true,
        })
        .into(),
        Event::FeedLoaded {
            feed: FeedId::Following,
            rows: vec![Row::new(
                RowKind::FollowedLive,
                FOLLOWED.iter().map(|l| Tile::Stream(live(l))).collect(),
            )],
        }
        .into(),
    ];
    run(&s, msgs).0
}

/// Watching `main` in the single player, playing.
fn watching() -> AppState {
    let (s, _) = press(&home(), &[Intent::Select]);
    reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0
}

fn mv(s: &AppState) -> &Multiview {
    s.multiview.as_ref().expect("multiview runs")
}

/// A grid of 4: `main` with sound on slot 0, `a`, `b` and `c` muted, all settled and playing, the
/// focus on `main`.
fn grid_of_four() -> AppState {
    let (mut s, _) = press(&watching(), &[Intent::View, Intent::Select]);
    for i in 0..3 {
        if i > 0 {
            s = reduce(&s, Intent::Down).0;
        }
        s = reduce(&s, Intent::Select).0;
    }
    s = reduce(&s, Intent::Back).0;
    s = reduce(&s, Event::QualitySettleDue(s.last_settle_id)).0;
    let slots: Vec<Slot> = mv(&s).tiles.iter().flatten().map(|t| t.slot).collect();
    for slot in slots {
        s = reduce(&s, Event::PlaybackStarted { slot }).0;
    }
    // Focus on `main`, the stream with sound.
    s.multiview.as_mut().unwrap().focus = 0;
    s
}

fn health(slot: Slot, at_ms: u64, health: Health) -> Event {
    Event::PlayerHealth {
        slot,
        at_ms,
        health,
    }
}

/// Each stream's channel, rendition and whether its chip is amber, in place order.
fn tiles(s: &AppState) -> Vec<(String, Rendition, bool)> {
    mv(s)
        .tiles
        .iter()
        .flatten()
        .map(|t| (t.stream.channel.login.clone(), t.rendition, t.lowered))
        .collect()
}

fn played(cmds: &[Command]) -> Vec<(Slot, StreamKind)> {
    cmds.iter()
        .filter_map(|c| match c {
            Command::Play { slot, stream, .. } => Some((*slot, *stream)),
            _ => None,
        })
        .collect()
}

fn tile(rendition: Rendition) -> StreamKind {
    StreamKind::Tile {
        rendition,
        rate: FrameRate::Light,
    }
}

#[test]
fn a_struggling_tile_lowers_the_muted_tiles_at_once_with_a_toast() {
    let s = grid_of_four();
    assert!(tiles(&s)
        .iter()
        .all(|(_, r, low)| *r == Rendition::P360 && !low));
    let muted = mv(&s).tile(1).unwrap().slot;

    let (s, cmds) = run(&s, vec![health(muted, 0, STALL).into()]);
    // The three muted tiles reload at 160p now, without waiting for the settle timer.
    let mut want: Vec<(Slot, StreamKind)> = mv(&s)
        .tiles
        .iter()
        .flatten()
        .filter(|t| t.slot != MAIN_SLOT)
        .map(|t| (t.slot, tile(Rendition::P160)))
        .collect();
    want.sort_by_key(|(slot, _)| *slot);
    let mut got = played(&cmds);
    got.sort_by_key(|(slot, _)| *slot);
    assert_eq!(got, want);
    assert_eq!(
        tiles(&s),
        [
            ("main".into(), Rendition::P360, false),
            ("a".into(), Rendition::P160, true),
            ("b".into(), Rendition::P160, true),
            ("c".into(), Rendition::P160, true),
        ]
    );
    assert_eq!(s.toasts[0].kind, ToastKind::SlowNetwork { muted: true });
    assert!(cmds.contains(&Command::ExpireToast { id: s.toasts[0].id }));
}

#[test]
fn the_sound_goes_down_last_and_the_toast_comes_once() {
    let s = grid_of_four();
    let (s, _) = run(&s, vec![health(1, 0, STALL).into()]);
    // The muted tiles reloaded; they play again.
    let (s, _) = run(
        &s,
        (1..4)
            .map(|slot| Event::PlaybackStarted { slot }.into())
            .collect(),
    );
    // The muted ones are at 160p: the next step is the stream with sound.
    let (s, cmds) = run(&s, vec![health(MAIN_SLOT, STEP_GAP_MS, STALL).into()]);
    assert_eq!(played(&cmds), [(MAIN_SLOT, tile(Rendition::P160))]);
    assert_eq!(tiles(&s)[0], ("main".into(), Rendition::P160, true));
    assert_eq!(s.toasts.len(), 1, "the toast is shown once");
    assert_eq!(s.net.steps(Tier::Sound), 1);
}

#[test]
fn loading_and_paused_streams_are_not_judged() {
    let s = grid_of_four();
    // A tile that is loading again (retried, reloaded) has an empty buffer by nature.
    let mut loading = s.clone();
    loading.multiview.as_mut().unwrap().tiles[1]
        .as_mut()
        .unwrap()
        .state = Playback::Loading;
    let slot = mv(&loading).tile(1).unwrap().slot;
    let (after, cmds) = run(&loading, vec![health(slot, 0, STALL).into()]);
    assert!(cmds.is_empty() && !after.net.is_limited());

    // Full screen, paused: the stream with sound only fills its buffer.
    let (paused, _) = press(&s, &[Intent::Select, Intent::Select]);
    assert_eq!(paused.screen, Screen::Player);
    assert!(paused.paused);
    let (after, cmds) = run(&paused, vec![health(MAIN_SLOT, 0, STALL).into()]);
    assert!(played(&cmds).is_empty() && !after.net.is_limited());

    // A slot no tile plays on.
    let (after, _) = run(&s, vec![health(7, 0, STALL).into()]);
    assert!(!after.net.is_limited());
}

#[test]
fn calm_brings_the_quality_back_the_sound_first() {
    let s = grid_of_four();
    let (s, _) = run(&s, vec![health(1, 0, STALL).into()]);
    let (s, _) = run(
        &s,
        (1..4)
            .map(|slot| Event::PlaybackStarted { slot }.into())
            .collect(),
    );
    let (s, _) = run(&s, vec![health(MAIN_SLOT, STEP_GAP_MS, STALL).into()]);
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    assert!(tiles(&s).iter().all(|(_, r, _)| *r == Rendition::P160));

    // Two calm minutes: the stream with sound comes back to 360p first.
    let calm: Vec<Msg> = (1..=CALM_MS / 1_000)
        .map(|k| health(MAIN_SLOT, STEP_GAP_MS + k * 1_000, OK).into())
        .collect();
    let (s, cmds) = run(&s, calm);
    assert_eq!(played(&cmds), [(MAIN_SLOT, tile(Rendition::P360))]);
    assert_eq!(tiles(&s)[0], ("main".into(), Rendition::P360, false));
    assert!(tiles(&s)[1..]
        .iter()
        .all(|(_, r, low)| *r == Rendition::P160 && *low));
}

#[test]
fn the_limit_outlasts_leaving_multiview() {
    let s = grid_of_four();
    let (s, _) = run(&s, vec![health(1, 0, STALL).into()]);
    let (left, _) = press(&s, &[Intent::Back]);
    assert!(left.multiview.is_none());
    assert_eq!(left.net.steps(Tier::MutedSmall), 1);
    assert_eq!(left.screen, Screen::Home);

    // *Watch the 4 together* on `main`'s card: the muted tiles start at 160p, amber.
    let (again, _) = press(&left, &[Intent::View]);
    let popup = again.popup.as_ref().expect("the card's popup");
    let row = popup
        .rows
        .iter()
        .position(|r| *r == PopupRow::WatchPicked)
        .expect("Watch the 4 together");
    let mut again = again;
    again.popup.as_mut().unwrap().focus = row;
    let (again, cmds) = press(&again, &[Intent::Select]);
    let sound = mv(&again).sound_tile().unwrap().slot;
    let muted: Vec<StreamKind> = played(&cmds)
        .into_iter()
        .filter(|(slot, _)| *slot != sound)
        .map(|(_, k)| k)
        .collect();
    assert_eq!(muted, [tile(Rendition::P160); 3]);
    assert!(played(&cmds).contains(&(sound, tile(Rendition::P360))));
    assert!(tiles(&again)[1..].iter().all(|(_, _, low)| *low));
}

#[test]
fn the_single_player_lowers_its_stream_and_says_so() {
    let s = watching();
    let ceiling = Rendition::ceiling(s.quality());
    assert_eq!(s.stream_kind(), StreamKind::Video(s.quality()));

    let (s, cmds) = run(&s, vec![health(MAIN_SLOT, 0, STALL).into()]);
    let want = StreamKind::Tile {
        rendition: ceiling.lowered(1),
        rate: FrameRate::Smooth,
    };
    assert_eq!(played(&cmds), [(MAIN_SLOT, want)]);
    // The old quality plays on until the lower one's first frame.
    assert_eq!(s.playback, Playback::Playing);
    assert_eq!(s.toasts[0].kind, ToastKind::SlowNetwork { muted: false });
    assert_eq!(s.stream_kind(), want);
}

#[test]
fn the_single_player_is_not_judged_paused_or_sound_only_or_from_other_slots() {
    let s = watching();
    let (paused, _) = press(&s, &[Intent::Select]);
    assert!(paused.paused);
    let (after, cmds) = run(&paused, vec![health(MAIN_SLOT, 0, STALL).into()]);
    assert!(cmds.is_empty() && !after.net.is_limited());

    let (after, cmds) = run(&s, vec![health(4, 0, STALL).into()]);
    assert!(cmds.is_empty() && !after.net.is_limited());

    let mut audio = s.clone();
    audio.audio_only = true;
    let (after, cmds) = run(&audio, vec![health(MAIN_SLOT, 0, STALL).into()]);
    assert!(cmds.is_empty() && !after.net.is_limited());
}

#[test]
fn the_single_player_at_the_lowest_quality_cannot_go_lower() {
    let mut s = watching();
    s.session.quality = Some(Quality::Worst);
    let (after, cmds) = run(&s, vec![health(MAIN_SLOT, 0, STALL).into()]);
    assert!(played(&cmds).is_empty());
    assert!(!after.net.is_limited());
    assert!(after.toasts.is_empty());
}
