//! Playback state: loading, playing, offline, failure, retry, pause and the HUD pulse.

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

/// Home with one stream, then Select: the Player screen, loading.
fn watching() -> (AppState, Vec<Command>) {
    let (s, _) = init();
    let rows = vec![Row::new(RowKind::TopLive, vec![stream("chan")])];
    let (s, _) = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    );
    reduce(&s, Intent::Select)
}

fn play(channel: &str) -> Command {
    Command::Play {
        channel: ChannelRef::twitch(channel),
        stream: StreamKind::Video(Quality::Best),
        latency: Latency::Low,
    }
}

fn ev(s: &AppState, e: Event) -> AppState {
    reduce(s, e).0
}

#[test]
fn selecting_a_stream_starts_loading_and_plays() {
    let (s, cmds) = watching();
    assert_eq!(s.screen, Screen::Player);
    assert_eq!(s.playback, Playback::Loading);
    assert!(!s.paused);
    assert!(cmds.contains(&play("chan")));
}

#[test]
fn the_configured_quality_is_requested() {
    let (mut s, _) = init();
    s.settings.quality = Quality::P720;
    let rows = vec![Row::new(RowKind::TopLive, vec![stream("chan")])];
    let s = ev(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    );
    let (_, cmds) = reduce(&s, Intent::Select);
    assert!(cmds.contains(&Command::Play {
        channel: ChannelRef::twitch("chan"),
        stream: StreamKind::Video(Quality::P720),
        latency: Latency::Low
    }));
}

#[test]
fn the_first_frame_means_playing() {
    let (s, _) = watching();
    let s = ev(&s, Event::PlaybackStarted);
    assert_eq!(s.playback, Playback::Playing);
}

#[test]
fn an_offline_channel_shows_the_offline_state() {
    let (s, _) = watching();
    let s = ev(
        &s,
        Event::StreamOffline {
            channel: ChannelRef::twitch("chan"),
        },
    );
    assert_eq!(s.playback, Playback::Offline);
}

#[test]
fn a_resolver_failure_carries_its_message() {
    let (s, _) = watching();
    let s = ev(
        &s,
        Event::PlaybackFailed {
            channel: ChannelRef::twitch("chan"),
            message: "Streamlink is not installed".into(),
        },
    );
    assert_eq!(
        s.playback,
        Playback::Failed("Streamlink is not installed".into())
    );
}

#[test]
fn answers_for_another_channel_are_dropped() {
    let (s, _) = watching();
    let other = ev(
        &s,
        Event::StreamOffline {
            channel: ChannelRef::twitch("other"),
        },
    );
    assert_eq!(other.playback, Playback::Loading);
    let other = ev(
        &s,
        Event::PlaybackFailed {
            channel: ChannelRef::twitch("other"),
            message: "x".into(),
        },
    );
    assert_eq!(other.playback, Playback::Loading);
}

#[test]
fn late_answers_after_leaving_are_dropped() {
    let (s, _) = watching();
    let (s, cmds) = reduce(&s, Intent::Back);
    assert_eq!(
        cmds,
        vec![
            Command::StopPlayback,
            Command::LeaveChat,
            Command::StopHypeTrain
        ]
    );
    assert_eq!(s.playback, Playback::Idle);
    for e in [
        Event::PlaybackStarted,
        Event::PlaybackEnded,
        Event::PlaybackError("x".into()),
        Event::StreamOffline {
            channel: ChannelRef::twitch("chan"),
        },
    ] {
        assert_eq!(ev(&s, e).playback, Playback::Idle);
    }
}

#[test]
fn the_stream_ending_while_playing_is_offline() {
    let (s, _) = watching();
    let s = ev(&s, Event::PlaybackStarted);
    let s = ev(&s, Event::PlaybackEnded);
    assert_eq!(s.playback, Playback::Offline);
    // An end while still loading is not trusted (it belongs to an earlier stream).
    let (s, _) = watching();
    assert_eq!(ev(&s, Event::PlaybackEnded).playback, Playback::Loading);
}

#[test]
fn a_player_error_fails_loading_and_playing() {
    let (s, _) = watching();
    let failed = ev(&s, Event::PlaybackError("bad".into()));
    assert_eq!(failed.playback, Playback::Failed("bad".into()));
    let playing = ev(
        &ev(&s, Event::PlaybackStarted),
        Event::PlaybackError("x".into()),
    );
    assert_eq!(playing.playback, Playback::Failed("x".into()));
}

#[test]
fn x_and_a_retry_a_failed_or_offline_stream() {
    let (s, _) = watching();
    let offline = Event::StreamOffline {
        channel: ChannelRef::twitch("chan"),
    };
    for (bad, intent) in [
        (Event::PlaybackError("bad".into()), Intent::Retry),
        (Event::PlaybackError("bad".into()), Intent::Select),
        (offline, Intent::Retry),
    ] {
        let failed = ev(&s, bad);
        let (again, cmds) = reduce(&failed, intent);
        assert_eq!(again.playback, Playback::Loading);
        assert_eq!(cmds, vec![play("chan")]);
    }
}

#[test]
fn retry_does_nothing_while_loading_or_playing() {
    let (s, _) = watching();
    let (after, cmds) = reduce(&s, Intent::Retry);
    assert_eq!(after.playback, Playback::Loading);
    assert!(cmds.is_empty());
    let playing = ev(&s, Event::PlaybackStarted);
    let (after, cmds) = reduce(&playing, Intent::Retry);
    assert_eq!(after.playback, Playback::Playing);
    assert!(cmds.is_empty());
}

#[test]
fn a_pauses_and_resumes_a_live_stream() {
    let (s, _) = watching();
    let s = ev(&s, Event::PlaybackStarted);
    let (s, cmds) = reduce(&s, Intent::Select);
    assert!(s.paused);
    assert_eq!(cmds, vec![Command::SetPaused(true)]);
    let (s, cmds) = reduce(&s, Intent::Select);
    assert!(!s.paused);
    assert_eq!(cmds, vec![Command::SetPaused(false)]);
}

#[test]
fn a_does_nothing_while_loading() {
    let (s, _) = watching();
    let (after, cmds) = reduce(&s, Intent::Select);
    assert_eq!(after.playback, Playback::Loading);
    assert!(!after.paused);
    assert!(cmds.is_empty());
}

#[test]
fn the_hud_pulses_on_input_and_state_changes() {
    let (s, _) = watching();
    let base = s.hud_pulse;
    let (s, _) = reduce(&s, Intent::Up);
    assert!(s.hud_pulse != base, "a direction shows the HUD");
    let before = s.hud_pulse;
    let s = ev(&s, Event::PlaybackStarted);
    assert!(s.hud_pulse != before, "the first frame shows the HUD");
    let before = s.hud_pulse;
    let (s, _) = reduce(&s, Intent::ToggleChat);
    assert!(s.hud_pulse != before);
}

fn offline(s: &AppState) -> (AppState, Vec<Command>) {
    reduce(
        s,
        Event::StreamOffline {
            channel: ChannelRef::twitch("chan"),
        },
    )
}

#[test]
fn going_offline_schedules_a_recheck() {
    let (s, _) = watching();
    let (s, cmds) = offline(&s);
    assert_eq!(cmds, vec![Command::ScheduleOfflineRecheck { id: 1 }]);
    // A stream that ends while playing too (the 24 h restart).
    let (s, _) = reduce(&s, Intent::Retry);
    let s = ev(&s, Event::PlaybackStarted);
    let (s, cmds) = reduce(&s, Event::PlaybackEnded);
    assert_eq!(s.playback, Playback::Offline);
    assert_eq!(cmds, vec![Command::ScheduleOfflineRecheck { id: 2 }]);
}

#[test]
fn a_recheck_looks_quietly_and_plays_when_the_stream_is_back() {
    let (s, _) = watching();
    let (s, _) = offline(&s);
    let (s, cmds) = reduce(&s, Event::OfflineRecheckDue(1));
    assert_eq!(cmds, vec![play("chan")]);
    // The offline screen stays up while it looks.
    assert_eq!(s.playback, Playback::Offline);
    assert!(s.rechecking);
    // A second timer while looking does not ask twice.
    assert!(reduce(&s, Event::OfflineRecheckDue(1)).1.is_empty());
    let s = ev(&s, Event::PlaybackStarted);
    assert_eq!(s.playback, Playback::Playing);
    assert!(!s.rechecking);
}

#[test]
fn a_failed_recheck_stays_offline_and_looks_again() {
    let (s, _) = watching();
    let (s, _) = offline(&s);
    let (looking, _) = reduce(&s, Event::OfflineRecheckDue(1));
    for answer in [
        Event::StreamOffline {
            channel: ChannelRef::twitch("chan"),
        },
        Event::PlaybackFailed {
            channel: ChannelRef::twitch("chan"),
            message: "timeout".into(),
        },
        Event::PlaybackError("bad".into()),
    ] {
        let (s, cmds) = reduce(&looking, answer);
        assert_eq!(s.playback, Playback::Offline);
        assert!(!s.rechecking);
        assert_eq!(cmds, vec![Command::ScheduleOfflineRecheck { id: 2 }]);
    }
}

#[test]
fn stale_or_late_rechecks_do_nothing() {
    let (s, _) = watching();
    let (s, _) = offline(&s);
    // An older timer.
    assert!(reduce(&s, Event::OfflineRecheckDue(0)).1.is_empty());
    // After a manual retry, the stream is loading: no recheck.
    let (retrying, _) = reduce(&s, Intent::Retry);
    assert!(reduce(&retrying, Event::OfflineRecheckDue(1)).1.is_empty());
    // After leaving the player.
    let (left, _) = reduce(&s, Intent::Back);
    let (left, cmds) = reduce(&left, Event::OfflineRecheckDue(1));
    assert!(cmds.is_empty());
    assert_eq!(left.playback, Playback::Idle);
}

#[test]
fn back_resets_pause() {
    let (s, _) = watching();
    let s = ev(&s, Event::PlaybackStarted);
    let (s, _) = reduce(&s, Intent::Select);
    assert!(s.paused);
    let (s, _) = reduce(&s, Intent::Back);
    assert!(!s.paused);
    assert!(s.now_playing.is_none());
}
