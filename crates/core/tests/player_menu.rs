//! The player menu (Start on the Player screen) and the stats box: options that last until the
//! app quits and are never saved.

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

/// Home with two streams.
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

/// The first stream, playing.
fn playing() -> AppState {
    let (s, _) = reduce(&home(), Intent::Select);
    reduce(&s, Event::PlaybackStarted { slot: 0 }).0
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

fn stats() -> PlayerStats {
    PlayerStats {
        width: Some(1920),
        height: Some(1080),
        fps_milli: Some(60_000),
        ..PlayerStats::default()
    }
}

fn no_saves(cmds: &[Command]) {
    assert!(
        !cmds.iter().any(|c| matches!(c, Command::SaveSettings(_))),
        "the player menu saved settings: {cmds:?}"
    );
}

#[test]
fn start_opens_the_menu_and_b_or_start_closes_it() {
    let (s, cmds) = reduce(&playing(), Intent::Menu);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::Quality));
    assert!(cmds.is_empty());
    assert_eq!(s.screen, Screen::Player);

    let (closed, cmds) = reduce(&s, Intent::Back);
    assert_eq!(closed.player_menu, None);
    assert_eq!(
        closed.screen,
        Screen::Player,
        "B closes the menu, not the player"
    );
    assert!(cmds.is_empty());

    let (closed, _) = reduce(&s, Intent::Menu);
    assert_eq!(closed.player_menu, None);
}

#[test]
fn up_and_down_move_between_rows_and_stop_at_the_ends() {
    let (s, _) = run(&playing(), &[Intent::Menu, Intent::Up]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::Quality));
    let (s, _) = run(&s, &[Intent::Down; 12]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::ChannelVideos));
    let (s, _) = reduce(&s, Intent::Up);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::WriteChat));
}

#[test]
fn the_menu_takes_the_buttons_that_otherwise_act_on_the_stream() {
    let (s, _) = reduce(&playing(), Intent::Menu);
    // Down moves in the menu instead of pausing or leaving.
    let (s, _) = reduce(&s, Intent::Down);
    assert!(!s.paused);
    assert_eq!(s.screen, Screen::Player);
    // The HUD stays up while the menu is used.
    let before = s.hud_pulse;
    let (s, _) = reduce(&s, Intent::Down);
    assert_ne!(s.hud_pulse, before);
}

#[test]
fn changing_quality_reloads_the_stream_and_is_not_saved() {
    let (s, cmds) = run(&playing(), &[Intent::Menu, Intent::Right]);
    assert_eq!(s.quality(), Quality::P1080);
    assert_eq!(
        s.settings.quality,
        Quality::Best,
        "the saved setting is untouched"
    );
    // The old quality stays on screen until the new one's first frame.
    assert_eq!(s.playback, Playback::Playing);
    assert_eq!(
        cmds,
        vec![Command::Play {
            slot: 0,
            channel: ChannelRef::twitch("chan"),
            stream: StreamKind::Video(Quality::P1080),
            latency: Latency::Low,
        }]
    );
    // Left steps back, wrapping around.
    let (s, cmds) = run(&s, &[Intent::Left, Intent::Left]);
    assert_eq!(s.quality(), Quality::Worst);
    no_saves(&cmds);
}

#[test]
fn the_menu_quality_is_kept_for_the_next_stream() {
    let (s, _) = run(&playing(), &[Intent::Menu, Intent::Left]);
    assert_eq!(s.quality(), Quality::Worst);
    let (s, _) = reduce(&s, Intent::Back); // close the menu
    let (s, _) = reduce(&s, Intent::Back); // leave the player
    assert_eq!(s.player_menu, None);
    let (_, cmds) = run(&s, &[Intent::Right, Intent::Select]);
    assert!(cmds.contains(&Command::Play {
        slot: 0,
        channel: ChannelRef::twitch("other"),
        stream: StreamKind::Video(Quality::Worst),
        latency: Latency::Low,
    }));
}

#[test]
fn a_quality_change_on_the_settings_screen_replaces_the_menu_pick() {
    let (s, _) = run(&playing(), &[Intent::Menu, Intent::Left]);
    let (s, _) = run(&s, &[Intent::Back, Intent::Back]);
    let (s, _) = reduce(&s, Intent::RailPick(RailEntry::Settings));
    // Down to the quality row, then A: Best -> 1080p, saved.
    let (s, cmds) = run(&s, &[Intent::Down, Intent::Down, Intent::Select]);
    assert_eq!(cmds, vec![Command::SaveSettings(s.settings)]);
    assert_eq!(s.settings.quality, Quality::P1080);
    assert_eq!(s.quality(), Quality::P1080);
}

#[test]
fn changing_latency_reloads_the_stream_with_it() {
    let (s, cmds) = run(&playing(), &[Intent::Menu, Intent::Down, Intent::Down]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::Latency));
    assert!(cmds.is_empty());
    let (s, cmds) = reduce(&s, Intent::Right);
    assert_eq!(s.session.latency, Latency::Normal);
    assert_eq!(
        cmds,
        vec![Command::Play {
            slot: 0,
            channel: ChannelRef::twitch("chan"),
            stream: StreamKind::Video(Quality::Best),
            latency: Latency::Normal,
        }]
    );
}

#[test]
fn hardware_decoding_and_zoom_apply_without_a_reload() {
    let (s, cmds) = run(&playing(), &[Intent::Menu, Intent::Down, Intent::Right]);
    assert_eq!(s.session.hwdec, Hwdec::Off);
    assert_eq!(s.playback, Playback::Playing, "no reload");
    assert_eq!(
        cmds,
        vec![Command::SetPlayerOption(PlayerOption::Hwdec(Hwdec::Off))]
    );

    let (s, cmds) = run(&s, &[Intent::Down, Intent::Down, Intent::Select]);
    assert_eq!(s.session.zoom, Zoom::Fill);
    assert_eq!(
        cmds,
        vec![Command::SetPlayerOption(PlayerOption::Zoom(Zoom::Fill))]
    );
    no_saves(&cmds);
}

#[test]
fn nothing_in_the_menu_ever_saves() {
    let (_, cmds) = run(
        &playing(),
        &[
            Intent::Menu,
            Intent::Right,
            Intent::Down,
            Intent::Right,
            Intent::Down,
            Intent::Left,
            Intent::Down,
            Intent::Right,
            Intent::Down,
            Intent::Right,
            Intent::Select,
        ],
    );
    no_saves(&cmds);
}

/// Opens the menu and turns the stats box on.
fn stats_on() -> (AppState, Vec<Command>) {
    let (s, _) = run(&playing(), &[Intent::Menu, Intent::Down, Intent::Down]);
    let (s, _) = run(&s, &[Intent::Down, Intent::Down]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::Stats));
    reduce(&s, Intent::Right)
}

#[test]
fn the_stats_row_starts_and_stops_the_reports() {
    let (s, cmds) = stats_on();
    assert!(s.session.stats);
    assert_eq!(cmds, vec![Command::SetStats { slot: 0, on: true }]);

    let s = reduce(&s, Event::PlayerStats(stats())).0;
    assert_eq!(s.stats, Some(stats()));

    let (s, cmds) = reduce(&s, Intent::Left);
    assert!(!s.session.stats);
    assert_eq!(s.stats, None, "turning it off hides the numbers at once");
    assert_eq!(cmds, vec![Command::SetStats { slot: 0, on: false }]);
}

#[test]
fn stats_are_only_accepted_while_on_and_on_the_player() {
    // Off: a late report is dropped.
    let s = reduce(&playing(), Event::PlayerStats(stats())).0;
    assert_eq!(s.stats, None);
    // On, but off the Player screen.
    let (s, _) = stats_on();
    let (s, cmds) = run(&s, &[Intent::Back, Intent::Back]);
    assert_eq!(s.screen, Screen::Home);
    assert!(
        cmds.contains(&Command::SetStats { slot: 0, on: false }),
        "{cmds:?}"
    );
    let s = reduce(&s, Event::PlayerStats(stats())).0;
    assert_eq!(s.stats, None);
}

#[test]
fn stats_stay_on_into_the_next_stream() {
    let (s, _) = stats_on();
    let s = reduce(&s, Event::PlayerStats(stats())).0;
    let (s, _) = run(&s, &[Intent::Back, Intent::Back]);
    assert!(s.session.stats, "kept after leaving");
    assert_eq!(s.stats, None, "the old numbers are gone");

    let (s, cmds) = run(&s, &[Intent::Right, Intent::Select]);
    assert_eq!(s.screen, Screen::Player);
    assert!(
        cmds.contains(&Command::SetStats { slot: 0, on: true }),
        "{cmds:?}"
    );
    let s = reduce(&s, Event::PlayerStats(stats())).0;
    assert_eq!(s.stats, Some(stats()));
}

#[test]
fn a_reload_keeps_the_numbers_until_the_new_quality_reports() {
    let (s, _) = stats_on();
    let s = reduce(&s, Event::PlayerStats(stats())).0;
    // Up to the quality row and change it: the new quality loads behind the old one.
    let (s, cmds) = run(
        &s,
        &[
            Intent::Up,
            Intent::Up,
            Intent::Up,
            Intent::Up,
            Intent::Right,
        ],
    );
    assert_eq!(s.playback, Playback::Playing);
    assert_eq!(s.stats, Some(stats()));
    assert!(matches!(cmds[0], Command::Play { slot: 0, .. }), "{cmds:?}");

    // A stream that does not play starts over, without the old numbers.
    let failed = reduce(
        &s,
        Event::PlaybackError {
            slot: 0,
            message: "x".into(),
        },
    )
    .0;
    let (s, _) = reduce(&failed, Intent::Back); // close the menu
    let (s, cmds) = reduce(&s, Intent::Retry);
    assert_eq!(s.playback, Playback::Loading);
    assert_eq!(s.stats, None);
    assert!(cmds.contains(&Command::SetStats { slot: 0, on: true }));
}

#[test]
fn leaving_the_player_closes_the_menu_but_keeps_the_choices() {
    let (s, _) = run(&playing(), &[Intent::Menu, Intent::Down, Intent::Right]);
    assert_eq!(s.session.hwdec, Hwdec::Off);
    let (s, _) = run(&s, &[Intent::Back, Intent::Back]);
    assert_eq!(s.screen, Screen::Home);
    assert_eq!(s.player_menu, None);
    assert_eq!(s.session.hwdec, Hwdec::Off);
    // The next stream opens with the menu closed.
    let (s, _) = reduce(&s, Intent::Select);
    assert_eq!(s.player_menu, None);
}

/// Opens the menu on the audio-only row.
fn on_audio_row() -> AppState {
    let (s, _) = run(&playing(), &[Intent::Menu]);
    let (s, _) = run(&s, &[Intent::Down; 5]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::AudioOnly));
    s
}

#[test]
fn audio_only_reloads_the_stream_as_sound_only() {
    let s = on_audio_row();
    assert!(!s.audio_only, "every stream starts as video");
    let (s, cmds) = reduce(&s, Intent::Right);
    assert!(s.audio_only);
    assert_eq!(s.playback, Playback::Playing);
    assert_eq!(
        cmds,
        vec![Command::Play {
            slot: 0,
            channel: ChannelRef::twitch("chan"),
            stream: StreamKind::AudioOnly,
            latency: Latency::Low,
        }]
    );
    no_saves(&cmds);
}

#[test]
fn turning_audio_only_off_goes_back_to_the_session_quality() {
    // Pick 720p in the menu first, then go audio only.
    let (s, _) = run(&playing(), &[Intent::Menu, Intent::Right, Intent::Right]);
    assert_eq!(s.quality(), Quality::P720);
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    let (s, _) = run(&s, &[Intent::Down; 5]);
    let (s, _) = reduce(&s, Intent::Select);
    assert!(s.audio_only);
    assert_eq!(s.quality(), Quality::P720, "the quality is kept");
    // Left toggles as well.
    let (s, cmds) = reduce(&s, Intent::Left);
    assert!(!s.audio_only);
    assert_eq!(
        cmds,
        vec![Command::Play {
            slot: 0,
            channel: ChannelRef::twitch("chan"),
            stream: StreamKind::Video(Quality::P720),
            latency: Latency::Low,
        }]
    );
}

#[test]
fn the_quality_row_is_locked_while_audio_only_is_on() {
    let (s, _) = reduce(&on_audio_row(), Intent::Right);
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    let (s, _) = run(&s, &[Intent::Up; 6]);
    assert_eq!(s.player_menu, Some(PlayerMenuRow::Quality));
    let (s, cmds) = run(&s, &[Intent::Right, Intent::Left, Intent::Select]);
    assert!(cmds.is_empty(), "{cmds:?}");
    assert_eq!(s.quality(), Quality::Best);
    assert_eq!(s.playback, Playback::Playing, "no reload");
}

#[test]
fn a_retry_stays_audio_only() {
    let (s, _) = reduce(&on_audio_row(), Intent::Right);
    // The sound-only stream fails once it plays (a failed resolve leaves the old one playing).
    let s = reduce(
        &s,
        Event::PlaybackError {
            slot: 0,
            message: "x".into(),
        },
    )
    .0;
    let (s, _) = reduce(&s, Intent::Back); // close the menu
    let (_, cmds) = reduce(&s, Intent::Retry);
    assert!(cmds.contains(&Command::Play {
        slot: 0,
        channel: ChannelRef::twitch("chan"),
        stream: StreamKind::AudioOnly,
        latency: Latency::Low,
    }));
}

#[test]
fn the_next_stream_starts_as_video_again() {
    let (s, _) = reduce(&on_audio_row(), Intent::Right);
    let (s, _) = run(&s, &[Intent::Back, Intent::Back]);
    assert_eq!(s.screen, Screen::Home);
    assert!(!s.audio_only, "reset on leaving");
    let (s, cmds) = run(&s, &[Intent::Right, Intent::Select]);
    assert!(!s.audio_only);
    assert!(cmds.contains(&Command::Play {
        slot: 0,
        channel: ChannelRef::twitch("other"),
        stream: StreamKind::Video(Quality::Best),
        latency: Latency::Low,
    }));
}

#[test]
fn y_still_toggles_chat_with_the_menu_open() {
    let (s, _) = run(&playing(), &[Intent::Menu, Intent::ToggleChat]);
    assert!(s.chat_open);
    assert!(s.player_menu.is_some());
}
