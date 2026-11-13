//! Seeking with ◀ ▶ in a video (vod-plan.md section 2, *Seeking, in detail*): taps adding up and
//! settling, hold detection, the scrub's speed-up and cap, release, reversing, B cancelling, and
//! the lost-release guards. The reducer has no clock, so these tests keep one: a script of
//! presses and releases at given milliseconds, and the timers the reducer asks for
//! (`SettleSeek`, `HoldCheck`, `ScrubTick`) fire at the time their delay says, stale ones
//! included, as they would in the app.

use htpc_core::seek::{HOLD_AFTER_MS, SCRUB_MAX_TICKS, SCRUB_TICK_MS, SEEK_SETTLE_MS, TAP_JOIN_MS};
use htpc_core::*;

const MIN: u64 = 60_000;
/// 4:12:08.
const LENGTH: u64 = 15_128_000;
/// 1:12:40, where the video is when the script starts.
const AT: u64 = 4_360_000;

fn video(length_s: u32) -> Video {
    Video {
        id: "1".into(),
        channel: ChannelRef::twitch("sleepy"),
        channel_name: "Sleepy Fox".into(),
        kind: VideoKind::Archive,
        title: "night 12".into(),
        category: "Hollow Knight".into(),
        length_s,
        created_at: 0,
        views: 0,
        tint: Rgb::default(),
        thumbnail: None,
        muted: Vec::new(),
        sub_only: false,
        recording: false,
    }
}

/// The channel page with one video of `length_s`.
fn page(length_s: u32) -> AppState {
    let (s, _) = init();
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(
                RowKind::TopLive,
                vec![Tile::Channel(Channel {
                    channel: ChannelRef::twitch("sleepy"),
                    name: "sleepy".into(),
                    tint: Rgb::default(),
                })],
            )],
        },
    )
    .0;
    let (s, _) = reduce(&s, Intent::Select);
    let info = ChannelInfo {
        channel: ChannelRef::twitch("sleepy"),
        user_id: "77".into(),
        name: "Sleepy Fox".into(),
        avatar: None,
        live: None,
        last_live_at: None,
        loaded_at: 0,
    };
    reduce(
        &s,
        Event::ChannelPageLoaded {
            info: Box::new(info),
            rows: vec![Row::new(
                RowKind::PastBroadcasts,
                vec![Tile::Video(video(length_s))],
            )],
        },
    )
    .0
}

fn health_at(pos_ms: u64) -> Event {
    Event::PlayerHealth {
        slot: MAIN_SLOT,
        at_ms: 0,
        health: Health {
            stalled: false,
            buffer_ms: 60_000,
            pos_ms: Some(pos_ms),
            length_ms: None,
        },
    }
}

/// The video playing at `pos_ms`.
fn playing(length_s: u32, pos_ms: u64) -> AppState {
    let (s, _) = reduce(&page(length_s), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    reduce(&s, health_at(pos_ms)).0
}

/// The reducer with a clock: input at given times, timers firing when due.
struct Sim {
    s: AppState,
    now: u64,
    /// Timers asked for and not fired yet: when they are due, and what they send.
    timers: Vec<(u64, Event)>,
    /// Every other command, with the time it was sent.
    out: Vec<(u64, Command)>,
}

impl Sim {
    fn new(s: AppState) -> Self {
        Self {
            s,
            now: 0,
            timers: Vec::new(),
            out: Vec::new(),
        }
    }

    fn apply(&mut self, msg: Msg) {
        let (next, cmds) = reduce(&self.s, msg);
        self.s = next;
        for cmd in cmds {
            let timer = match cmd {
                Command::SettleSeek { id, ms } => Some((ms, Event::SeekSettleDue(id))),
                Command::HoldCheck { id } => Some((HOLD_AFTER_MS, Event::HoldDue(id))),
                Command::ScrubTick { id } => Some((SCRUB_TICK_MS, Event::ScrubTickDue(id))),
                other => {
                    self.out.push((self.now, other));
                    None
                }
            };
            if let Some((ms, event)) = timer {
                self.timers.push((self.now + ms, event));
            }
        }
    }

    /// Fires every timer due by `t`, in order, then moves the clock to `t`.
    fn run_until(&mut self, t: u64) {
        loop {
            let next = self
                .timers
                .iter()
                .enumerate()
                .filter(|(_, (due, _))| *due <= t)
                .min_by_key(|(i, (due, _))| (*due, *i))
                .map(|(i, _)| i);
            let Some(i) = next else {
                break;
            };
            let (due, event) = self.timers.remove(i);
            self.now = due;
            self.apply(event.into());
        }
        self.now = t;
    }

    /// `msg` at time `t` (after the timers due before it).
    fn at(&mut self, t: u64, msg: impl Into<Msg>) -> &mut Self {
        self.run_until(t);
        self.apply(msg.into());
        self
    }

    fn press(&mut self, t: u64, dir: Dir) -> &mut Self {
        self.at(t, dir.intent())
    }

    fn release(&mut self, t: u64, dir: Dir) -> &mut Self {
        self.at(t, Intent::Released(dir))
    }

    /// The seeks sent: when, and where to.
    fn seeks(&self) -> Vec<(u64, u64)> {
        self.out
            .iter()
            .filter_map(|(t, c)| match c {
                Command::Seek { to_ms, .. } => Some((*t, *to_ms)),
                _ => None,
            })
            .collect()
    }

    /// The pauses and resumes sent: when, and which.
    fn pauses(&self) -> Vec<(u64, bool)> {
        self.out
            .iter()
            .filter_map(|(t, c)| match c {
                Command::SetPaused { paused, .. } => Some((*t, *paused)),
                _ => None,
            })
            .collect()
    }

    fn video(&self) -> &VideoSession {
        self.s.video.as_ref().expect("a video")
    }
}

use Dir::{Left, Right};

/// One scripted case: what happens, then the seeks and pauses it must have sent by `until`.
struct Case {
    name: &'static str,
    start_ms: u64,
    script: &'static [(u64, Act)],
    until: u64,
    seeks: &'static [(u64, u64)],
    pauses: &'static [(u64, bool)],
    /// Where the video is at the end.
    pos_ms: u64,
}

#[derive(Clone, Copy)]
enum Act {
    Press(Dir),
    Release(Dir),
    B,
    A,
}

fn play(case: &Case) {
    let mut sim = Sim::new(playing(15_128, case.start_ms));
    for (t, act) in case.script {
        match act {
            Act::Press(dir) => sim.press(*t, *dir),
            Act::Release(dir) => sim.release(*t, *dir),
            Act::B => sim.at(*t, Intent::Back),
            Act::A => sim.at(*t, Intent::Select),
        };
    }
    sim.run_until(case.until);
    assert_eq!(sim.seeks(), case.seeks, "{}: seeks", case.name);
    assert_eq!(sim.pauses(), case.pauses, "{}: pauses", case.name);
    assert_eq!(sim.video().pos_ms, case.pos_ms, "{}: position", case.name);
    assert!(
        sim.video().seeking.is_idle(),
        "{}: still seeking",
        case.name
    );
    assert_eq!(
        sim.s.screen,
        Screen::Player,
        "{}: left the player",
        case.name
    );
    assert!(
        sim.out
            .iter()
            .all(|(_, c)| matches!(c, Command::Seek { .. } | Command::SetPaused { .. })),
        "{}: {:?}",
        case.name,
        sim.out
    );
}

#[test]
fn taps_and_holds_as_scripted() {
    const S: u64 = SEEK_SETTLE_MS;
    let cases = [
        Case {
            name: "one tap: 10 s, sent 300 ms after it is let go",
            start_ms: AT,
            script: &[(0, Act::Press(Right)), (80, Act::Release(Right))],
            until: 2_000,
            seeks: &[(80 + S, AT + 10_000)],
            pauses: &[],
            pos_ms: AT + 10_000,
        },
        Case {
            name: "one tap back",
            start_ms: AT,
            script: &[(0, Act::Press(Left)), (90, Act::Release(Left))],
            until: 2_000,
            seeks: &[(90 + S, AT - 10_000)],
            pauses: &[],
            pos_ms: AT - 10_000,
        },
        Case {
            name: "three quick taps add up into one seek",
            start_ms: AT,
            script: &[
                (0, Act::Press(Right)),
                (80, Act::Release(Right)),
                (200, Act::Press(Right)),
                (280, Act::Release(Right)),
                (400, Act::Press(Right)),
                (480, Act::Release(Right)),
            ],
            until: 2_000,
            seeks: &[(480 + S, AT + 30_000)],
            pauses: &[],
            pos_ms: AT + 30_000,
        },
        Case {
            name: "a tap 500 ms after the last still adds up, and sends the total",
            start_ms: AT,
            script: &[
                (0, Act::Press(Right)),
                (50, Act::Release(Right)),
                (550, Act::Press(Right)),
                (600, Act::Release(Right)),
            ],
            until: 2_000,
            seeks: &[(50 + S, AT + 10_000), (600 + S, AT + 20_000)],
            pauses: &[],
            pos_ms: AT + 20_000,
        },
        Case {
            name: "taps further apart than 600 ms are two runs, the second from the first's spot",
            start_ms: AT,
            script: &[
                (0, Act::Press(Right)),
                (50, Act::Release(Right)),
                (800, Act::Press(Right)),
                (850, Act::Release(Right)),
            ],
            until: 3_000,
            seeks: &[(50 + S, AT + 10_000), (850 + S, AT + 20_000)],
            pauses: &[],
            pos_ms: AT + 20_000,
        },
        Case {
            name: "back and forth that comes to nothing sends nothing",
            start_ms: AT,
            script: &[
                (0, Act::Press(Right)),
                (60, Act::Release(Right)),
                (150, Act::Press(Left)),
                (210, Act::Release(Left)),
            ],
            until: 2_000,
            seeks: &[],
            pauses: &[],
            pos_ms: AT,
        },
        Case {
            name: "taps back stop at the start",
            start_ms: 15_000,
            script: &[
                (0, Act::Press(Left)),
                (60, Act::Release(Left)),
                (150, Act::Press(Left)),
                (210, Act::Release(Left)),
            ],
            until: 2_000,
            seeks: &[(210 + S, 0)],
            pauses: &[],
            pos_ms: 0,
        },
        Case {
            name: "taps forward stop a second before the end",
            start_ms: LENGTH - 4_000,
            script: &[(0, Act::Press(Right)), (60, Act::Release(Right))],
            until: 2_000,
            seeks: &[(60 + S, LENGTH - 1_000)],
            pauses: &[],
            pos_ms: LENGTH - 1_000,
        },
        Case {
            // Held from 0: a hold at 400 (its first step at once), steps every 100 ms. Let go at
            // 1400, after the step due then: 11 steps, 8 of 1 s (ticks 0-7), then 3 of 2 s. The
            // jump comes 300 ms after the release. (Doubling every second, before 2026-11-13,
            // this was +12 s.)
            name: "a hold scrubs, speeding up after 0.8 s, and jumps on release",
            start_ms: AT,
            script: &[(0, Act::Press(Right)), (1_400, Act::Release(Right))],
            until: 3_000,
            seeks: &[(1_400 + S, AT + 14_000)],
            pauses: &[(400, true), (1_400 + S, false)],
            pos_ms: AT + 14_000,
        },
        Case {
            name: "a short hold back",
            start_ms: AT,
            script: &[(0, Act::Press(Left)), (650, Act::Release(Left))],
            until: 3_000,
            // Steps at 400, 500 and 600.
            seeks: &[(650 + S, AT - 3_000)],
            pauses: &[(400, true), (650 + S, false)],
            pos_ms: AT - 3_000,
        },
        Case {
            name: "B while scrubbing goes back to where it started, with no seek",
            start_ms: AT,
            script: &[
                (0, Act::Press(Right)),
                (1_000, Act::B),
                (1_100, Act::Release(Right)),
            ],
            until: 3_000,
            seeks: &[],
            pauses: &[(400, true), (1_000, false)],
            pos_ms: AT,
        },
        Case {
            name: "A while scrubbing jumps at once",
            start_ms: AT,
            script: &[
                (0, Act::Press(Right)),
                (720, Act::A),
                (900, Act::Release(Right)),
            ],
            until: 3_000,
            // Steps at 400, 500, 600, 700.
            seeks: &[(720, AT + 4_000)],
            pauses: &[(400, true), (720, false)],
            pos_ms: AT + 4_000,
        },
        Case {
            // Right held: steps at 400 ... 900 (6 s). Left pressed at 950 while Right is
            // still down: reverses, slowly again, its first step at once: 950, 1050 ... 1350
            // (5 s back). Right let go at 1000: not the button that drives it, ignored. Left
            // let go at 1380: the jump at 1680, to +1 s.
            name: "the other direction reverses the scrub",
            start_ms: AT,
            script: &[
                (0, Act::Press(Right)),
                (950, Act::Press(Left)),
                (1_000, Act::Release(Right)),
                (1_380, Act::Release(Left)),
            ],
            until: 3_000,
            seeks: &[(1_380 + S, AT + 1_000)],
            pauses: &[(400, true), (1_380 + S, false)],
            pos_ms: AT + 1_000,
        },
        Case {
            // A rocker D-pad lets go of Right before Left goes down. Within 300 ms the scrub
            // carries on the other way: +6 s (400 ... 900), then back from 1050: 1050 ... 1350.
            name: "a rocker D-pad reverses through the middle",
            start_ms: AT,
            script: &[
                (0, Act::Press(Right)),
                (980, Act::Release(Right)),
                (1_050, Act::Press(Left)),
                (1_380, Act::Release(Left)),
            ],
            until: 3_000,
            seeks: &[(1_380 + S, AT + 2_000)],
            pauses: &[(400, true), (1_380 + S, false)],
            pos_ms: AT + 2_000,
        },
        Case {
            // A tap (unsent), then a hold in the same run: the tap is sent when the hold starts,
            // and the scrub goes on from there.
            name: "a tap then a hold: the tap counts, the hold scrubs from there",
            start_ms: AT,
            script: &[
                (0, Act::Press(Right)),
                (80, Act::Release(Right)),
                (200, Act::Press(Right)),
                (750, Act::Release(Right)),
            ],
            until: 3_000,
            // Hold at 600: steps at 600 and 700.
            seeks: &[(600, AT + 10_000), (750 + S, AT + 12_000)],
            pauses: &[(600, true), (750 + S, false)],
            pos_ms: AT + 12_000,
        },
    ];
    for case in &cases {
        play(case);
    }
}

#[test]
fn a_new_hold_soon_after_a_scrub_picks_up_near_its_speed() {
    const S: u64 = SEEK_SETTLE_MS;
    // Every case starts with Right held from 0 to 2000: a hold at 400, then steps at 400 ...
    // 2000, 17 of them (ticks 0-16): 8 of 1 s, 8 of 2 s, 1 of 4 s, so +28 s, sent at 2300. Its
    // last step was 4 s, so a carried scrub starts on the 2 s steps (tick 8).
    const FIRST: (u64, u64) = (2_000 + S, AT + 28_000);
    assert_eq!(seek::SCRUB_CARRY_MS, 1_500);
    assert_eq!(seek::SCRUB_CARRY_PERCENT, 50);
    let cases = [
        Case {
            // 1 s after letting go: the hold at 3400 steps 2 s at 3400, 3500, 3600.
            name: "the same way within 1.5 s carries on near half the speed",
            start_ms: AT,
            script: &[
                (0, Act::Press(Right)),
                (2_000, Act::Release(Right)),
                (3_000, Act::Press(Right)),
                (3_600, Act::Release(Right)),
            ],
            until: 6_000,
            seeks: &[FIRST, (3_600 + S, AT + 34_000)],
            pauses: &[(400, true), (2_300, false), (3_400, true), (3_900, false)],
            pos_ms: AT + 34_000,
        },
        Case {
            // Pressed again during the release's wait: the scrub goes on, from the 2 s steps
            // (2100, 2200, 2300).
            name: "the same way within the release's wait carries on near half the speed",
            start_ms: AT,
            script: &[
                (0, Act::Press(Right)),
                (2_000, Act::Release(Right)),
                (2_100, Act::Press(Right)),
                (2_300, Act::Release(Right)),
            ],
            until: 6_000,
            seeks: &[(2_300 + S, AT + 34_000)],
            pauses: &[(400, true), (2_600, false)],
            pos_ms: AT + 34_000,
        },
        Case {
            // 1.6 s after letting go: the window closed at 3500. 1 s steps at 4000 ... 4200.
            name: "the same way after 1.5 s starts slowly",
            start_ms: AT,
            script: &[
                (0, Act::Press(Right)),
                (2_000, Act::Release(Right)),
                (3_600, Act::Press(Right)),
                (4_200, Act::Release(Right)),
            ],
            until: 6_000,
            seeks: &[FIRST, (4_200 + S, AT + 31_000)],
            pauses: &[(400, true), (2_300, false), (4_000, true), (4_500, false)],
            pos_ms: AT + 31_000,
        },
        Case {
            name: "the other way starts slowly",
            start_ms: AT,
            script: &[
                (0, Act::Press(Right)),
                (2_000, Act::Release(Right)),
                (3_000, Act::Press(Left)),
                (3_600, Act::Release(Left)),
            ],
            until: 6_000,
            seeks: &[FIRST, (3_600 + S, AT + 25_000)],
            pauses: &[(400, true), (2_300, false), (3_400, true), (3_900, false)],
            pos_ms: AT + 25_000,
        },
        Case {
            // B during the release's wait cancels the scrub: nothing to carry on.
            name: "after B cancelled the scrub, the same way starts slowly",
            start_ms: AT,
            script: &[
                (0, Act::Press(Right)),
                (2_000, Act::Release(Right)),
                (2_100, Act::B),
                (2_500, Act::Press(Right)),
                (3_100, Act::Release(Right)),
            ],
            until: 6_000,
            seeks: &[(3_100 + S, AT + 3_000)],
            pauses: &[(400, true), (2_100, false), (2_900, true), (3_400, false)],
            pos_ms: AT + 3_000,
        },
        Case {
            name: "after B while held, the same way starts slowly",
            start_ms: AT,
            script: &[
                (0, Act::Press(Right)),
                (2_000, Act::B),
                (2_050, Act::Release(Right)),
                (2_500, Act::Press(Right)),
                (3_100, Act::Release(Right)),
            ],
            until: 6_000,
            seeks: &[(3_100 + S, AT + 3_000)],
            pauses: &[(400, true), (2_000, false), (2_900, true), (3_400, false)],
            pos_ms: AT + 3_000,
        },
        Case {
            // A tap at 3000 (+10 s, sent at 3350), then a hold in its run: from the slowest.
            name: "a tap in between starts the next hold slowly",
            start_ms: AT,
            script: &[
                (0, Act::Press(Right)),
                (2_000, Act::Release(Right)),
                (3_000, Act::Press(Right)),
                (3_050, Act::Release(Right)),
                (3_500, Act::Press(Right)),
                (4_100, Act::Release(Right)),
            ],
            until: 6_000,
            seeks: &[FIRST, (3_050 + S, AT + 38_000), (4_100 + S, AT + 41_000)],
            pauses: &[(400, true), (2_300, false), (3_900, true), (4_400, false)],
            pos_ms: AT + 41_000,
        },
    ];
    for case in &cases {
        play(case);
    }
}

#[test]
fn a_carried_scrub_still_keeps_to_the_cap() {
    // 18 hours: the cap is 1%, 648 s a step.
    const CAP: u64 = 648_000;
    let mut sim = Sim::new(playing(18 * 3_600, 0));
    // Held 8.5 s: at the cap since 8 s in.
    sim.press(0, Right).release(8_900, Right);
    sim.run_until(9_500);
    assert!(sim.video().seeking.is_idle());
    // Again 600 ms after letting go: from 256 s a step (the fastest doubling at or under half
    // the cap), then 512 s, then the cap, never more.
    sim.press(9_500, Right);
    let mut at = sim.video().pos_ms;
    let mut steps = Vec::new();
    for t in (9_900..=12_900).step_by(100) {
        sim.run_until(t);
        let Seeking::Scrub(scrub) = sim.video().seeking else {
            panic!("not scrubbing at {t}");
        };
        steps.push(scrub.at_ms - at);
        at = scrub.at_ms;
    }
    assert_eq!(steps[0], 256_000);
    assert_eq!(steps[8], 512_000);
    assert!(steps.iter().all(|step| *step <= CAP), "{steps:?}");
    assert_eq!(steps.last(), Some(&CAP));
}

#[test]
fn the_chip_counts_while_the_run_lasts_and_goes_after_600_ms() {
    let mut sim = Sim::new(playing(15_128, AT));
    sim.press(0, Right).release(60, Right);
    sim.press(150, Right).release(210, Right);
    // The taps add up on screen before anything is sent.
    let run = *sim.video().seeking.run().expect("a run");
    assert_eq!(run.delta_ms, 20_000);
    assert_eq!(sim.video().shown_ms(), AT + 20_000);
    sim.run_until(210 + SEEK_SETTLE_MS);
    assert_eq!(sim.seeks(), [(510, AT + 20_000)]);
    assert!(sim.video().seeking.run().is_some(), "the chip stays");
    sim.run_until(210 + TAP_JOIN_MS);
    assert!(sim.video().seeking.is_idle(), "the run is over");
}

#[test]
fn the_video_plays_on_while_taps_add_up() {
    let mut sim = Sim::new(playing(15_128, AT));
    sim.press(0, Right).release(60, Right);
    sim.run_until(5_000);
    assert!(sim.pauses().is_empty());
    assert!(!sim.s.paused);
}

#[test]
fn a_hold_on_a_paused_video_leaves_it_paused() {
    let paused = reduce(&playing(15_128, AT), Intent::Select).0;
    let mut sim = Sim::new(paused);
    sim.press(0, Right).release(650, Right);
    sim.run_until(3_000);
    assert_eq!(sim.seeks(), [(650 + SEEK_SETTLE_MS, AT + 3_000)]);
    assert!(sim.pauses().is_empty(), "{:?}", sim.pauses());
    assert!(sim.s.paused);
}

#[test]
fn the_scrub_ramp_doubles_every_0_8_seconds_and_caps_at_one_percent() {
    // 18 hours: held for 15 s, the steps grow to 1% (648 s) after 8 s and stay there.
    let mut sim = Sim::new(playing(18 * 3_600, 0));
    sim.press(0, Right);
    sim.run_until(HOLD_AFTER_MS + 15_000);
    let Seeking::Scrub(scrub) = sim.video().seeking else {
        panic!("not scrubbing");
    };
    let ticks = scrub.ticks;
    assert_eq!(ticks, 151, "a step at the hold and ten a second after it");
    let expected: u64 = (0..ticks)
        .map(|t| seek::scrub_step(t, 18 * 3_600_000))
        .sum();
    assert_eq!(scrub.at_ms, expected);
    assert_eq!(seek::scrub_step(ticks - 1, 18 * 3_600_000), 648_000);
    // A 10 minute highlight: 6 s a tick at most, and the playhead stops short of the end.
    let mut sim = Sim::new(playing(600, 0));
    sim.press(0, Right);
    sim.run_until(HOLD_AFTER_MS + 15_000);
    let Seeking::Scrub(scrub) = sim.video().seeking else {
        panic!("not scrubbing");
    };
    let to_end: u64 = (0..scrub.ticks).map(|t| seek::scrub_step(t, 600_000)).sum();
    assert!(to_end > 599_000);
    assert_eq!(scrub.at_ms, 599_000, "never past the end guard");
}

#[test]
fn a_lost_release_ends_the_scrub_after_30_seconds() {
    let mut sim = Sim::new(playing(15_128, 0));
    sim.press(0, Right);
    // Ticks run 10 a second from the hold; after SCRUB_MAX_TICKS of them it lets go itself.
    let end = HOLD_AFTER_MS + u64::from(SCRUB_MAX_TICKS + 1) * SCRUB_TICK_MS;
    sim.run_until(end + 1_000);
    assert!(sim.video().seeking.is_idle());
    let seeks = sim.seeks();
    assert_eq!(seeks.len(), 1, "{seeks:?}");
    assert_eq!(seeks[0].0, end);
    assert_eq!(sim.pauses(), [(HOLD_AFTER_MS, true), (end, false)]);
    // No timer keeps running.
    sim.run_until(end + 60_000);
    assert_eq!(sim.seeks().len(), 1);
}

#[test]
fn losing_focus_while_holding_counts_as_a_release() {
    let mut s = playing(15_128, AT);
    s.settings.background_play = true;
    let mut sim = Sim::new(s);
    sim.press(0, Right);
    sim.at(650, Event::WindowFocus(false));
    sim.run_until(3_000);
    assert_eq!(sim.seeks(), [(650 + SEEK_SETTLE_MS, AT + 3_000)]);
    assert_eq!(sim.pauses(), [(400, true), (650 + SEEK_SETTLE_MS, false)]);
}

#[test]
fn losing_focus_without_background_play_keeps_it_paused_after_the_jump() {
    let mut sim = Sim::new(playing(15_128, AT));
    sim.press(0, Right);
    sim.at(650, Event::WindowFocus(false));
    sim.run_until(3_000);
    assert_eq!(sim.seeks(), [(650 + SEEK_SETTLE_MS, AT + 3_000)]);
    // Paused for the scrub, again for the background, and not resumed by the jump.
    assert_eq!(sim.pauses(), [(400, true), (650, true)]);
    assert!(sim.s.paused);
}

#[test]
fn a_press_whose_release_was_lost_counts_as_a_tap_when_pressed_again() {
    // Two presses of Right with no release in between, 100 ms apart: two taps.
    let mut sim = Sim::new(playing(15_128, AT));
    sim.press(0, Right).press(100, Right).release(150, Right);
    sim.run_until(2_000);
    assert_eq!(sim.seeks(), [(150 + SEEK_SETTLE_MS, AT + 20_000)]);
}

#[test]
fn a_seek_before_the_first_frame_goes_into_the_start_of_the_load() {
    let (loading, _) = reduce(&page(15_128), Intent::Select);
    let mut sim = Sim::new(loading);
    sim.press(0, Right).release(60, Right);
    // Held while loading: no scrub, it stays a press and becomes a tap.
    sim.press(200, Right).release(900, Right);
    sim.run_until(3_000);
    assert!(sim.seeks().is_empty());
    assert!(sim.pauses().is_empty());
    let plays: Vec<(u64, u64)> = sim
        .out
        .iter()
        .filter_map(|(t, c)| match c {
            Command::PlayVideo { start_ms, .. } => Some((*t, *start_ms)),
            _ => None,
        })
        .collect();
    assert_eq!(plays, [(900 + SEEK_SETTLE_MS, 20_000)]);
    assert_eq!(sim.s.playback, Playback::Loading);
}

#[test]
fn a_seek_while_another_quality_loads_moves_the_picture_and_the_new_load() {
    let s = playing(15_128, AT);
    let (s, _) = reduce(&s, Intent::Menu);
    let (s, _) = reduce(&s, Intent::Right);
    let (s, _) = reduce(&s, Intent::Back);
    assert_eq!(
        s.video.as_ref().unwrap().load,
        VideoLoad::Reloading { reports: 0 }
    );
    let mut sim = Sim::new(s);
    sim.press(0, Right).release(60, Right);
    sim.run_until(1_000);
    assert_eq!(
        sim.out,
        [
            (
                360,
                Command::Seek {
                    slot: MAIN_SLOT,
                    to_ms: AT + 10_000
                }
            ),
            (
                360,
                Command::PlayVideo {
                    slot: MAIN_SLOT,
                    target: VideoTarget::Video("1".into()),
                    start_ms: AT + 10_000,
                    stream: StreamKind::Video(Quality::P1080),
                }
            )
        ]
    );
}

#[test]
fn a_new_quality_that_never_shows_gives_seeks_back_to_the_player() {
    let s = playing(15_128, AT);
    let (s, _) = reduce(&s, Intent::Menu);
    let (s, _) = reduce(&s, Intent::Right);
    let (mut s, _) = reduce(&s, Intent::Back);
    for i in 0..seek::RELOAD_GIVE_UP_REPORTS {
        s = reduce(&s, health_at(AT + u64::from(i) * 1_000)).0;
    }
    assert_eq!(s.video.as_ref().unwrap().load, VideoLoad::Shown);
}

#[test]
fn reports_from_before_a_seek_do_not_pull_the_playhead_back() {
    let mut sim = Sim::new(playing(15_128, AT));
    sim.press(0, Right).release(60, Right);
    sim.run_until(1_000);
    assert_eq!(sim.video().pos_ms, AT + 10_000);
    // A report read before the seek landed: ignored.
    sim.at(1_100, health_at(AT + 1_000));
    assert_eq!(sim.video().pos_ms, AT + 10_000);
    // One near the new spot: taken.
    sim.at(2_000, health_at(AT + 10_400));
    assert_eq!(sim.video().pos_ms, AT + 10_400);
    // After that every report counts again.
    sim.at(3_000, health_at(AT + 900));
    assert_eq!(sim.video().pos_ms, AT + 900);
}

#[test]
fn a_seek_that_never_lands_is_believed_after_three_reports() {
    let mut sim = Sim::new(playing(15_128, AT));
    sim.press(0, Right).release(60, Right);
    sim.run_until(1_000);
    for (i, t) in [1_100, 2_100, 3_100].into_iter().enumerate() {
        sim.at(t, health_at(AT + i as u64 * 1_000));
        assert_eq!(sim.video().pos_ms, AT + 10_000, "report {i}");
    }
    sim.at(4_100, health_at(AT + 3_000));
    assert_eq!(sim.video().pos_ms, AT + 3_000);
}

#[test]
fn a_chapter_jump_replaces_taps_still_adding_up() {
    let s = playing(15_128, AT);
    let s = reduce(
        &s,
        Event::VideoDetails(VideoDetails {
            id: "1".into(),
            length_s: 15_128,
            chapters: vec![
                Chapter {
                    start_ms: 0,
                    length_ms: 0,
                    name: "a".into(),
                },
                Chapter {
                    start_ms: 80 * MIN,
                    length_ms: 0,
                    name: "b".into(),
                },
            ],
            muted: Vec::new(),
            previews: None,
        }),
    )
    .0;
    let mut sim = Sim::new(s);
    sim.press(0, Right).release(60, Right);
    sim.at(100, Intent::PageRight);
    sim.run_until(2_000);
    assert_eq!(sim.seeks(), [(100, 80 * MIN)]);
}

#[test]
fn the_menu_takes_the_d_pad_from_a_press() {
    let mut sim = Sim::new(playing(15_128, AT));
    sim.press(0, Right);
    sim.at(100, Intent::Menu);
    // Held past 400 ms, but the menu is open: it was a tap.
    sim.at(500, Intent::Down);
    sim.release(700, Right);
    sim.run_until(2_000);
    assert_eq!(sim.seeks(), [(100 + SEEK_SETTLE_MS, AT + 10_000)]);
    assert!(sim.pauses().is_empty());
    assert_eq!(sim.s.player_menu, Some(PlayerMenuRow::Speed));
}

#[test]
fn releases_do_nothing_anywhere_else() {
    let (home, _) = init();
    for dir in [Left, Right] {
        let (next, cmds) = reduce(&home, Intent::Released(dir));
        assert_eq!(next, home);
        assert!(cmds.is_empty());
    }
    let page = page(15_128);
    let (next, cmds) = reduce(&page, Intent::Released(Right));
    assert_eq!(next, page);
    assert!(cmds.is_empty());
    // A live stream ignores them too.
    let live = Tile::Stream(Stream {
        channel: ChannelRef::twitch("fox"),
        user_id: String::new(),
        title: String::new(),
        category: String::new(),
        viewers: 1,
        tint: Rgb::default(),
        thumbnail: None,
    });
    let (s, _) = init();
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(RowKind::TopLive, vec![live])],
        },
    )
    .0;
    let (s, _) = reduce(&s, Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: MAIN_SLOT }).0;
    let (next, cmds) = reduce(&s, Intent::Released(Left));
    assert_eq!(next, s);
    assert!(cmds.is_empty());
}

#[test]
fn a_release_does_not_count_as_a_button_for_still_watching() {
    let s = playing(15_128, AT);
    let (s, _) = reduce(&s, Intent::Menu);
    let (s, _) = reduce(&s, Intent::Up);
    let s = (0..8).fold(s, |s, _| reduce(&s, Intent::Down).0);
    let (s, _) = reduce(&s, Intent::Right);
    let (s, _) = reduce(&s, Intent::Back);
    let id = s.sleep.unwrap().id;
    let s = reduce(&s, Event::SleepTimerWarning { id }).0;
    let (next, cmds) = reduce(&s, Intent::Released(Right));
    assert!(next.sleep.unwrap().warning, "the prompt is still up");
    assert!(cmds.is_empty());
}

#[test]
fn stale_timers_do_nothing() {
    let s = playing(15_128, AT);
    for event in [
        Event::SeekSettleDue(0),
        Event::HoldDue(0),
        Event::ScrubTickDue(0),
        Event::SeekSettleDue(99),
    ] {
        let (next, cmds) = reduce(&s, event.clone());
        assert_eq!(next, s, "{event:?}");
        assert!(cmds.is_empty(), "{event:?}");
    }
}
