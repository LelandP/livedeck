//! Hype trains: the channel on screen is watched for one while the player is open, signed in or
//! not, and only answers about that channel count.

use htpc_core::*;

fn stream(channel: &str) -> Tile {
    Tile::Stream(Stream {
        channel: channel.into(),
        user_id: String::new(),
        title: format!("{channel} title"),
        category: "Cat".into(),
        viewers: 1_000,
        tint: Rgb::from_hex(0x3a5a40),
        thumbnail: None,
    })
}

fn home() -> AppState {
    let (s, _) = init();
    reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(RowKind::TopLive, vec![stream("ender")])],
        },
    )
    .0
}

fn train(level: u32) -> HypeTrain {
    HypeTrain {
        level,
        progress: 400,
        goal: 1800,
        seconds_left: 297,
    }
}

fn hype(channel: &str, train: Option<HypeTrain>) -> Event {
    Event::HypeTrain {
        channel: channel.into(),
        train,
    }
}

#[test]
fn opening_a_stream_watches_it_and_leaving_stops() {
    let (s, cmds) = reduce(&home(), Intent::Select);
    assert!(cmds.contains(&Command::WatchHypeTrain {
        channel: "ender".into()
    }));
    let (s, _) = reduce(&s, hype("ender", Some(train(2))));
    assert_eq!(s.hype_train, Some(train(2)));

    let (s, cmds) = reduce(&s, Intent::Back);
    assert!(cmds.contains(&Command::StopHypeTrain));
    assert_eq!(s.hype_train, None, "gone with the player");
}

#[test]
fn only_the_channel_on_screen_counts() {
    let (s, _) = reduce(&home(), Intent::Select);
    let (s, _) = reduce(&s, hype("someone_else", Some(train(3))));
    assert_eq!(s.hype_train, None);
    // Logins are case-insensitive.
    let (s, _) = reduce(&s, hype("Ender", Some(train(3))));
    assert_eq!(s.hype_train, Some(train(3)));
    // A late answer once the player is closed is dropped.
    let (s, _) = reduce(&s, Intent::Back);
    let (s, _) = reduce(&s, hype("ender", Some(train(4))));
    assert_eq!(s.hype_train, None);
}

#[test]
fn a_train_that_ends_goes_away() {
    let (s, _) = reduce(&home(), Intent::Select);
    let (s, _) = reduce(&s, hype("ender", Some(train(1))));
    let (s, _) = reduce(&s, hype("ender", None));
    assert_eq!(s.hype_train, None);
}

#[test]
fn following_a_raid_watches_the_new_channel_and_drops_the_old_train() {
    let (s, _) = init();
    let s = reduce(
        &s,
        Event::SignedIn(User {
            login: "viewer".into(),
            can_chat: true,
        }),
    )
    .0;
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(RowKind::TopLive, vec![stream("ender")])],
        },
    )
    .0;
    let (s, _) = reduce(&s, Intent::Select);
    let (s, _) = reduce(&s, Event::PlaybackStarted);
    let (s, _) = reduce(&s, hype("ender", Some(train(5))));
    let (s, _) = reduce(
        &s,
        Event::Raided {
            from_login: "ender".into(),
            to_login: "next_up".into(),
            to_name: "Next_Up".into(),
            viewers: 10,
        },
    );
    let (s, cmds) = reduce(&s, Intent::Select);
    assert_eq!(s.now_playing.as_ref().unwrap().channel, "next_up");
    assert_eq!(s.hype_train, None);
    assert!(cmds.contains(&Command::WatchHypeTrain {
        channel: "next_up".into()
    }));
}
