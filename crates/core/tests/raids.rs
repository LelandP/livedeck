//! Follow raids: the channel on screen raids another one, a card counts down, and A (or the end of
//! the countdown) moves the player there. Signed in only.

use htpc_core::*;

fn stream(channel: &str, user_id: &str) -> Tile {
    Tile::Stream(Stream {
        channel: channel.into(),
        user_id: user_id.into(),
        title: format!("{channel} title"),
        category: "Cat".into(),
        viewers: 1_000,
        tint: Rgb::from_hex(0x3a5a40),
        thumbnail: None,
    })
}

fn user() -> User {
    User {
        login: "viewer".into(),
    }
}

/// Home with one stream and one offline followed channel, signed in or not.
fn home(signed_in: bool) -> AppState {
    let (s, _) = init();
    let offline = Tile::Channel(Channel {
        channel: "sleepy".into(),
        name: "Sleepy".into(),
        tint: Rgb::default(),
    });
    let mut s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(
                RowKind::TopLive,
                vec![stream("ender", "100"), offline],
            )],
        },
    )
    .0;
    if signed_in {
        s = reduce(&s, Event::SignedIn(user())).0;
    }
    s
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

/// Watching `ender` (user id 100), playing.
fn watching(signed_in: bool) -> AppState {
    let (s, _) = reduce(&home(signed_in), Intent::Select);
    reduce(&s, Event::PlaybackStarted).0
}

fn raid(from: &str, to: &str) -> Event {
    Event::Raided {
        from_login: from.into(),
        to_login: to.into(),
        to_name: to.to_uppercase(),
        viewers: 1_234,
    }
}

fn offered() -> (AppState, u32) {
    let (s, cmds) = reduce(&watching(true), raid("ender", "next_up"));
    let id = s.raid.as_ref().expect("the card is open").id;
    assert_eq!(cmds, vec![Command::StartRaidCountdown { id, seconds: 10 }]);
    (s, id)
}

fn has(cmds: &[Command], pred: impl Fn(&Command) -> bool) -> bool {
    cmds.iter().any(pred)
}

#[test]
fn opening_a_stream_signed_in_watches_it_for_raids() {
    let (_, cmds) = reduce(&home(true), Intent::Select);
    assert!(cmds.contains(&Command::WatchRaids {
        broadcaster_id: "100".into()
    }));
}

#[test]
fn signed_out_never_subscribes_and_ignores_raids() {
    let s = watching(false);
    let (_, cmds) = reduce(&home(false), Intent::Select);
    assert!(!has(&cmds, |c| matches!(
        c,
        Command::WatchRaids { .. } | Command::FetchStreamInfo { .. }
    )));
    let (s, cmds) = reduce(&s, raid("ender", "next_up"));
    assert!(s.raid.is_none());
    assert!(cmds.is_empty());
    // Nor does leaving the player stop anything.
    let (_, cmds) = reduce(&s, Intent::Back);
    assert!(!cmds.contains(&Command::StopRaids));
}

#[test]
fn a_card_without_a_user_id_looks_it_up_first() {
    // The offline followed channel has no user id.
    let (s, cmds) = run(
        &home(true),
        vec![Intent::Right.into(), Intent::Select.into()],
    );
    assert!(cmds.contains(&Command::FetchStreamInfo {
        channel: "sleepy".into()
    }));
    assert!(!has(&cmds, |c| matches!(c, Command::WatchRaids { .. })));

    let info = Stream {
        channel: "sleepy".into(),
        user_id: "555".into(),
        title: String::new(),
        category: String::new(),
        viewers: 0,
        tint: Rgb::default(),
        thumbnail: None,
    };
    let (s, cmds) = reduce(&s, Event::StreamInfo(info.clone()));
    assert_eq!(
        cmds,
        vec![Command::WatchRaids {
            broadcaster_id: "555".into()
        }]
    );
    assert_eq!(s.now_playing.as_ref().unwrap().user_id, "555");
    // Knowing it already, a repeat changes nothing.
    let (_, cmds) = reduce(&s, Event::StreamInfo(info));
    assert!(cmds.is_empty());
}

#[test]
fn signing_in_while_watching_starts_the_watch() {
    let s = watching(false);
    let (_, cmds) = reduce(&s, Event::SignedIn(user()));
    assert!(cmds.contains(&Command::WatchRaids {
        broadcaster_id: "100".into()
    }));
}

#[test]
fn a_raid_from_another_channel_is_ignored() {
    let (s, cmds) = reduce(&watching(true), raid("someone_else", "next_up"));
    assert!(s.raid.is_none());
    assert!(cmds.is_empty());
}

#[test]
fn the_card_opens_while_playing_and_while_offline() {
    let (s, id) = offered();
    let offer = s.raid.unwrap();
    assert_eq!(offer.id, id);
    assert_eq!(offer.from, "ender");
    assert_eq!(offer.to_login, "next_up");
    assert_eq!(offer.to_name, "NEXT_UP");
    assert_eq!(offer.viewers, 1_234);
    assert_eq!(offer.seconds_left, RAID_SECONDS);

    // The stream already ended: the offline screen is up.
    let offline = reduce(&watching(true), Event::PlaybackEnded).0;
    assert_eq!(offline.playback, Playback::Offline);
    let (s, _) = reduce(&offline, raid("ender", "next_up"));
    assert!(s.raid.is_some());
}

#[test]
fn the_card_closes_the_menu_and_a_repeat_does_not_restart_it() {
    let s = reduce(&watching(true), Intent::Menu).0;
    assert!(s.player_menu.is_some());
    let (s, _) = reduce(&s, raid("ender", "next_up"));
    assert!(s.player_menu.is_none());
    let id = s.raid.as_ref().unwrap().id;
    let (s, cmds) = reduce(&s, raid("ender", "next_up"));
    assert!(cmds.is_empty());
    assert_eq!(s.raid.unwrap().id, id);
}

#[test]
fn ticks_count_down_and_stale_ids_are_dropped() {
    let (s, id) = offered();
    let (s, _) = reduce(&s, Event::RaidTick { id, left: 7 });
    assert_eq!(s.raid.as_ref().unwrap().seconds_left, 7);
    let (s, cmds) = run(
        &s,
        vec![
            Event::RaidTick {
                id: id + 1,
                left: 2,
            }
            .into(),
            Event::RaidCountdownDone { id: id + 1 }.into(),
        ],
    );
    assert_eq!(s.raid.as_ref().unwrap().seconds_left, 7);
    assert!(cmds.is_empty());
    assert_eq!(s.now_playing.unwrap().channel, "ender");
}

/// What following the raid must have done, whatever triggered it.
fn assert_followed(before: &AppState, s: &AppState, cmds: &[Command]) {
    assert!(s.raid.is_none());
    assert_eq!(s.screen, Screen::Player);
    assert_eq!(s.back_stack, before.back_stack, "replaced, not stacked");
    let p = s.now_playing.as_ref().unwrap();
    assert_eq!(p.channel, "next_up");
    assert!(p.user_id.is_empty() && p.title.is_empty());
    assert_eq!(s.playback, Playback::Loading);
    assert!(s.chat.is_empty());
    assert_eq!(s.chat_status, ChatStatus::Connecting);
    assert!(cmds.contains(&Command::StopRaids));
    assert!(cmds.contains(&Command::JoinChat {
        channel: "next_up".into()
    }));
    assert!(has(
        cmds,
        |c| matches!(c, Command::Play { channel, .. } if channel == "next_up")
    ));
    assert!(cmds.contains(&Command::FetchStreamInfo {
        channel: "next_up".into()
    }));
}

#[test]
fn the_end_of_the_countdown_follows_the_raid() {
    let (s, id) = offered();
    let (next, cmds) = reduce(&s, Event::RaidCountdownDone { id });
    assert_followed(&s, &next, &cmds);
}

#[test]
fn a_follows_at_once() {
    let (s, _) = offered();
    let (next, cmds) = reduce(&s, Intent::Select);
    assert_followed(&s, &next, &cmds);
    // A late tick or end of the old countdown finds nothing.
    let id = s.raid.unwrap().id;
    let (after, cmds) = reduce(&next, Event::RaidCountdownDone { id });
    assert!(cmds.is_empty());
    assert_eq!(after, next);
}

#[test]
fn b_after_following_goes_back_to_the_browse_screens() {
    let (s, _) = offered();
    let (s, _) = reduce(&s, Intent::Select);
    let (s, cmds) = reduce(&s, Intent::Back);
    assert_eq!(s.screen, Screen::Home);
    assert!(cmds.contains(&Command::StopRaids));
}

#[test]
fn the_new_channel_fills_in_and_is_watched_in_turn() {
    let (s, _) = offered();
    let (s, _) = reduce(&s, Intent::Select);
    let info = Stream {
        channel: "next_up".into(),
        user_id: "200".into(),
        title: "Just got raided".into(),
        category: "Art".into(),
        viewers: 2_000,
        tint: Rgb::default(),
        thumbnail: Some("https://cdn/next_up.jpg".into()),
    };
    let (s, cmds) = reduce(&s, Event::StreamInfo(info));
    let p = s.now_playing.unwrap();
    assert_eq!(
        (p.title.as_str(), p.category.as_str(), p.viewers),
        ("Just got raided", "Art", 2_000)
    );
    assert_eq!(
        cmds,
        vec![
            Command::FetchThumbnails {
                urls: vec!["https://cdn/next_up.jpg".into()]
            },
            Command::WatchRaids {
                broadcaster_id: "200".into()
            },
        ]
    );
}

#[test]
fn audio_only_and_the_session_carry_over() {
    let mut s = watching(true);
    s.audio_only = true;
    s.session.latency = Latency::Low;
    s.sleep = Some(SleepTimer {
        id: 3,
        minutes: 30,
        left_min: 12,
        warning: false,
    });
    let (s, _) = reduce(&s, raid("ender", "next_up"));
    let (s, cmds) = reduce(&s, Intent::Select);
    assert!(s.audio_only);
    assert_eq!(s.sleep.unwrap().id, 3);
    assert!(cmds.contains(&Command::Play {
        channel: "next_up".into(),
        stream: StreamKind::AudioOnly,
        latency: Latency::Low,
    }));
}

#[test]
fn b_stays_and_the_countdown_is_forgotten() {
    let (s, id) = offered();
    let (s, cmds) = reduce(&s, Intent::Back);
    assert!(s.raid.is_none());
    assert!(cmds.is_empty());
    assert_eq!(s.screen, Screen::Player, "B only closes the card");
    assert_eq!(s.now_playing.as_ref().unwrap().channel, "ender");
    let (after, cmds) = reduce(&s, Event::RaidCountdownDone { id });
    assert!(cmds.is_empty());
    assert_eq!(after.now_playing.unwrap().channel, "ender");
}

#[test]
fn other_buttons_are_ignored_while_the_card_is_open() {
    let (open, _) = offered();
    let (s, cmds) = run(
        &open,
        vec![
            Intent::Up.into(),
            Intent::Down.into(),
            Intent::Left.into(),
            Intent::Right.into(),
            Intent::ToggleChat.into(),
            Intent::Retry.into(),
            Intent::Menu.into(),
        ],
    );
    assert!(cmds.is_empty(), "{cmds:?}");
    assert_eq!(s, open);
}

#[test]
fn leaving_the_player_or_signing_out_clears_the_offer() {
    let (s, _) = offered();
    let mut timed = s.clone();
    timed.sleep = Some(SleepTimer {
        id: 9,
        minutes: 15,
        left_min: 0,
        warning: false,
    });
    let (left, cmds) = reduce(&timed, Event::SleepTimerFired { id: 9 });
    assert_eq!(left.screen, Screen::Home);
    assert!(left.raid.is_none());
    assert!(cmds.contains(&Command::StopRaids));

    let (out, cmds) = reduce(&s, Event::SignedOut);
    assert!(out.raid.is_none());
    assert!(cmds.contains(&Command::DisconnectRaids));
}

#[test]
fn stream_info_for_another_channel_is_ignored() {
    let s = watching(true);
    let other = Stream {
        channel: "elsewhere".into(),
        user_id: "9".into(),
        title: "x".into(),
        category: "y".into(),
        viewers: 1,
        tint: Rgb::default(),
        thumbnail: None,
    };
    let (after, cmds) = reduce(&s, Event::StreamInfo(other));
    assert_eq!(after, s);
    assert!(cmds.is_empty());
}
