//! The sleep timer (player menu): it runs on wall-clock time in the app, warns 60 s before the
//! end, and then stops the stream and suspends the device.

use htpc_core::*;

fn stream(channel: &str) -> Tile {
    Tile::Stream(Stream {
        channel: channel.into(),
        title: format!("{channel} title"),
        category: "Cat".into(),
        viewers: 1_000,
        tint: Rgb::default(),
        thumbnail: None,
    })
}

fn home() -> AppState {
    let (s, _) = init();
    let rows = vec![Row::new(
        RowKind::TopLive,
        vec![stream("chan"), stream("other")],
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

fn playing() -> AppState {
    let (s, _) = reduce(&home(), Intent::Select);
    reduce(&s, Event::PlaybackStarted).0
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

/// The player menu, on the sleep timer row.
fn on_sleep_row() -> AppState {
    let mut intents = vec![Intent::Menu];
    intents.extend([Intent::Down; 6]);
    let (s, _) = run(&playing(), &intents);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::SleepTimer));
    s
}

fn timer(s: &AppState) -> SleepTimer {
    s.sleep.expect("a sleep timer runs")
}

#[test]
fn the_steps_go_through_off_both_ways() {
    let mut m = None;
    let mut seen = Vec::new();
    for _ in 0..7 {
        m = SleepTimer::cycled(m, true);
        seen.push(m);
    }
    assert_eq!(
        seen,
        [
            Some(15),
            Some(30),
            Some(45),
            Some(60),
            Some(90),
            Some(120),
            None
        ]
    );
    assert_eq!(SleepTimer::cycled(None, false), Some(120));
    assert_eq!(SleepTimer::cycled(Some(15), false), None);
}

#[test]
fn setting_the_timer_starts_it_and_changing_it_starts_it_again() {
    let (s, cmds) = reduce(&on_sleep_row(), Intent::Right);
    let t = timer(&s);
    assert_eq!((t.minutes, t.left_min, t.warning), (15, 15, false));
    assert_eq!(
        cmds,
        [Command::StartSleepTimer {
            id: t.id,
            minutes: 15
        }]
    );

    let (s2, cmds) = reduce(&s, Intent::Select);
    let t2 = timer(&s2);
    assert_eq!(t2.minutes, 30);
    assert!(t2.id > t.id, "a new timer gets a new id");
    assert_eq!(
        cmds,
        [Command::StartSleepTimer {
            id: t2.id,
            minutes: 30
        }]
    );
}

#[test]
fn stepping_back_to_off_cancels_it() {
    let (s, _) = reduce(&on_sleep_row(), Intent::Right);
    let (s, cmds) = reduce(&s, Intent::Left);
    assert_eq!(s.sleep, None);
    assert_eq!(cmds, [Command::CancelSleepTimer]);
    // Ids keep going up after a cancel.
    let before = s.last_sleep_id;
    let (s, _) = reduce(&s, Intent::Right);
    assert!(timer(&s).id > before);
}

#[test]
fn the_timer_is_kept_when_the_stream_is_left_or_changed() {
    let (s, _) = reduce(&on_sleep_row(), Intent::Right);
    let id = timer(&s).id;
    let (s, cmds) = run(&s, &[Intent::Back, Intent::Back]);
    assert_eq!(s.screen, Screen::Home);
    assert_eq!(timer(&s).id, id);
    assert!(!cmds.iter().any(|c| matches!(
        c,
        Command::CancelSleepTimer | Command::StartSleepTimer { .. }
    )));
}

#[test]
fn ticks_update_the_minutes_left_and_old_ids_are_ignored() {
    let (s, _) = reduce(&on_sleep_row(), Intent::Right);
    let id = timer(&s).id;
    let (s, cmds) = reduce(&s, Event::SleepTimerTick { id, left_min: 12 });
    assert!(cmds.is_empty());
    assert_eq!(timer(&s).left_min, 12);

    // An older timer's events change nothing.
    let old = id - 1;
    for event in [
        Event::SleepTimerTick {
            id: old,
            left_min: 3,
        },
        Event::SleepTimerWarning { id: old },
        Event::SleepTimerFired { id: old },
    ] {
        let (after, cmds) = reduce(&s, event.clone());
        assert_eq!(after, s, "{event:?}");
        assert!(cmds.is_empty(), "{event:?}");
    }
}

#[test]
fn events_without_a_timer_are_ignored() {
    let s = playing();
    let (after, cmds) = reduce(&s, Event::SleepTimerFired { id: 1 });
    assert_eq!(after, s);
    assert!(cmds.is_empty());
}

#[test]
fn during_the_warning_any_button_only_extends() {
    let (s, _) = reduce(&on_sleep_row(), Intent::Right);
    let (s, _) = reduce(&s, Intent::Back); // close the menu
    let id = timer(&s).id;
    let (warned, cmds) = reduce(&s, Event::SleepTimerWarning { id });
    assert!(cmds.is_empty());
    assert!(timer(&warned).warning);
    assert_eq!(timer(&warned).left_min, 0);

    for intent in [
        Intent::Select,
        Intent::Back,
        Intent::ToggleChat,
        Intent::Menu,
        Intent::Up,
    ] {
        let (after, cmds) = reduce(&warned, intent);
        let t = timer(&after);
        assert!(!t.warning, "{intent:?}");
        assert_eq!((t.minutes, t.left_min), (15, 15), "{intent:?}");
        assert!(t.id > id, "{intent:?}");
        assert_eq!(
            cmds,
            [Command::StartSleepTimer {
                id: t.id,
                minutes: 15
            }],
            "{intent:?}"
        );
        // Nothing else happened: still watching, not paused, chat and menu as they were.
        assert_eq!(after.screen, Screen::Player, "{intent:?}");
        assert!(!after.paused, "{intent:?}");
        assert_eq!(after.chat_open, warned.chat_open, "{intent:?}");
        assert_eq!(after.player_menu, None, "{intent:?}");
    }
}

#[test]
fn firing_on_the_player_goes_back_and_suspends() {
    let (s, _) = reduce(&on_sleep_row(), Intent::Right);
    let id = timer(&s).id;
    let (s, _) = reduce(&s, Event::SleepTimerWarning { id });
    let (s, cmds) = reduce(&s, Event::SleepTimerFired { id });
    assert_eq!(s.sleep, None);
    assert_eq!(s.screen, Screen::Home);
    assert_eq!(s.now_playing, None);
    assert_eq!(s.player_menu, None);
    assert!(cmds.contains(&Command::StopPlayback));
    assert!(cmds.contains(&Command::LeaveChat));
    assert_eq!(cmds.last(), Some(&Command::Suspend));
}

#[test]
fn firing_elsewhere_only_suspends() {
    let (s, _) = reduce(&on_sleep_row(), Intent::Right);
    let (s, _) = run(&s, &[Intent::Back, Intent::Back]);
    let id = timer(&s).id;
    let (after, cmds) = reduce(&s, Event::SleepTimerFired { id });
    assert_eq!(cmds, [Command::Suspend]);
    assert_eq!(after.sleep, None);
    assert_eq!(after.screen, Screen::Home);
}
