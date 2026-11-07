//! Background play: switching to another app pauses the stream (unless the setting says to keep
//! playing) and coming back resumes it, but never undoes a pause the user made.

use htpc_core::*;

fn stream(channel: &str) -> Tile {
    Tile::Stream(Stream {
        channel: ChannelRef::twitch(channel),
        user_id: String::new(),
        title: format!("{channel} title"),
        category: "Cat".into(),
        viewers: 1_000,
        tint: Rgb::default(),
        thumbnail: None,
    })
}

fn home() -> AppState {
    let (s, _) = init();
    let rows = vec![Row::new(RowKind::TopLive, vec![stream("chan")])];
    reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    )
    .0
}

/// The Player screen, still loading.
fn loading() -> AppState {
    reduce(&home(), Intent::Select).0
}

/// The Player screen with the stream playing.
fn playing() -> AppState {
    reduce(&loading(), Event::PlaybackStarted { slot: 0 }).0
}

fn focus(s: &AppState, focused: bool) -> (AppState, Vec<Command>) {
    reduce(s, Event::WindowFocus(focused))
}

#[test]
fn switching_away_pauses_and_coming_back_resumes() {
    let (s, cmds) = focus(&playing(), false);
    assert!(s.paused);
    assert!(s.in_background);
    assert_eq!(
        cmds,
        [Command::SetPaused {
            slot: 0,
            paused: true
        }]
    );

    let (s, cmds) = focus(&s, true);
    assert!(!s.paused);
    assert!(!s.in_background);
    assert_eq!(
        cmds,
        [Command::SetPaused {
            slot: 0,
            paused: false
        }]
    );
}

#[test]
fn with_background_play_on_nothing_changes() {
    let mut s = playing();
    s.settings.background_play = true;
    let (s, cmds) = focus(&s, false);
    assert!(!s.paused);
    assert!(cmds.is_empty());
    let (s, cmds) = focus(&s, true);
    assert!(!s.paused);
    assert!(cmds.is_empty());
}

#[test]
fn a_pause_by_the_user_stays_after_coming_back() {
    let (s, _) = reduce(&playing(), Intent::Select);
    assert!(s.paused);
    let (s, cmds) = focus(&s, false);
    assert!(cmds.is_empty(), "already paused");
    let (s, cmds) = focus(&s, true);
    assert!(s.paused);
    assert!(cmds.is_empty());
}

#[test]
fn a_stream_that_starts_in_the_background_is_paused_at_once() {
    let (s, cmds) = focus(&loading(), false);
    assert!(cmds.is_empty(), "nothing plays yet");
    let (s, cmds) = reduce(&s, Event::PlaybackStarted { slot: 0 });
    assert_eq!(s.playback, Playback::Playing);
    assert!(s.paused);
    assert!(cmds.contains(&Command::SetPaused {
        slot: 0,
        paused: true
    }));
    let (s, cmds) = focus(&s, true);
    assert!(!s.paused);
    assert_eq!(
        cmds,
        [Command::SetPaused {
            slot: 0,
            paused: false
        }]
    );
}

#[test]
fn off_the_player_focus_changes_do_nothing() {
    let (s, cmds) = focus(&home(), false);
    assert!(cmds.is_empty());
    let (_, cmds) = focus(&s, true);
    assert!(cmds.is_empty());
}

#[test]
fn leaving_the_stream_forgets_the_background_pause() {
    let (s, _) = focus(&playing(), false);
    let (s, _) = reduce(&s, Intent::Back);
    assert!(!s.background_paused);
    let (_, cmds) = focus(&s, true);
    assert!(!cmds.contains(&Command::SetPaused {
        slot: 0,
        paused: false
    }));
}

#[test]
fn the_setting_toggles_and_is_saved() {
    let (s, _) = reduce(&home(), Intent::RailPick(RailEntry::Settings));
    let mut s = s;
    for _ in 0..4 {
        s = reduce(&s, Intent::Down).0;
    }
    assert_eq!(s.settings_focus, SettingsRow::BackgroundPlay);
    let (s, cmds) = reduce(&s, Intent::Select);
    assert!(s.settings.background_play);
    assert_eq!(cmds, [Command::SaveSettings(s.settings)]);
    let (s, cmds) = reduce(&s, Intent::Left);
    assert!(!s.settings.background_play);
    assert_eq!(cmds, [Command::SaveSettings(s.settings)]);
}
