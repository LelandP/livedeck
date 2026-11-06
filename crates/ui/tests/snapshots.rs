//! `insta` snapshots of the view-model for each Browse state. The view-model is rendered as plain
//! text (one line per card) so a diff in review shows exactly which card changed.
//!
//! Review changes with `cargo insta review`, or accept them with `INSTA_UPDATE=always cargo test`.

use htpc_core::{
    grid_rows, init, reduce, AppState, Category, ChatKind, ChatLayout, ChatMessage, ChatSpan,
    ChatStatus, Event, FeedId, Intent, Moderation, PageOutcome, Paging, PlayerMenuRow, PlayerStats,
    RailEntry, Rgb, Row, RowKind, Stream, Tile,
};
use htpc_ui::{view_model, CardVm, ViewModel};

fn stream(channel: &str, viewers: u64, image: bool) -> Tile {
    Tile::Stream(Stream {
        channel: channel.into(),
        title: format!("{channel} title"),
        category: "VALORANT".into(),
        viewers,
        tint: Rgb::from_hex(0x7a2e2e),
        thumbnail: image.then(|| format!("https://cdn/{channel}.jpg")),
    })
}

fn category(id: &str, name: &str) -> Tile {
    Tile::Category(Category {
        id: id.into(),
        name: name.into(),
        tint: Rgb::from_hex(0x6b5a2d),
        thumbnail: Some(format!("https://cdn/{id}.jpg")),
    })
}

fn card(c: &CardVm) -> String {
    if c.loading {
        return "[skeleton]".into();
    }
    if c.retry {
        return "[retry]".into();
    }
    let mut out = format!("[{}", c.channel);
    if !c.viewers.is_empty() {
        out += &format!(" | {} | {} viewers", c.category, c.viewers);
    }
    if !c.thumb_url.is_empty() {
        out += " | img";
    }
    out + "]"
}

fn render(vm: &ViewModel) -> String {
    let mut out = format!(
        "screen={:?} heading={:?} focus=({}, {}) rail_current={}\n",
        vm.screen, vm.heading, vm.sel_row, vm.sel_col, vm.rail_current
    );
    out += &format!(
        "select={:?} retry={} back={} empty={} error={:?}\n",
        vm.select_label, vm.can_retry, vm.can_back, vm.feed_empty, vm.feed_error
    );
    for (i, row) in vm.rows.iter().enumerate() {
        let cards: Vec<_> = row.cards.iter().map(card).collect();
        out += &format!(
            "row {i} {:?} {:?}: {}\n",
            row.shape,
            row.title,
            cards.join(" ")
        );
    }
    out
}

fn send(s: &AppState, intents: &[Intent]) -> AppState {
    intents.iter().fold(s.clone(), |s, i| reduce(&s, *i).0)
}

fn home_loaded() -> AppState {
    let rows = vec![
        Row::new(
            RowKind::Featured,
            vec![stream("grandmaster_k", 48_100, true)],
        ),
        Row::new(
            RowKind::FollowedLive,
            vec![
                stream("nightowl_dev", 4_200, true),
                stream("marrow", 1_900, false),
            ],
        ),
        Row::new(
            RowKind::TopLive,
            vec![
                stream("tidewatch", 31_500, true),
                stream("brickby", 22_400, true),
            ],
        ),
        Row::new(
            RowKind::Categories,
            vec![
                category("509658", "Just Chatting"),
                category("516575", "VALORANT"),
            ],
        ),
    ];
    let (s, _) = init();
    reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    )
    .0
}

#[test]
fn home_while_loading() {
    let (s, _) = init();
    insta::assert_snapshot!(render(&view_model(&s)));
}

#[test]
fn home_loaded_with_the_hero_focused() {
    insta::assert_snapshot!(render(&view_model(&home_loaded())));
}

#[test]
fn home_with_a_next_page_loading() {
    let tiles: Vec<Tile> = (0..8)
        .map(|i| stream(&format!("s{i}"), 100, false))
        .collect();
    let row = Row::new(RowKind::TopLive, tiles).with_paging(Paging::More("c".into()));
    let (s, _) = init();
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![row],
        },
    )
    .0;
    insta::assert_snapshot!(render(&view_model(&send(&s, &[Intent::Right; 4]))));
}

#[test]
fn home_with_a_failed_page() {
    let s = send(&home_loaded(), &[Intent::Down, Intent::Down]);
    // Row 2 (Top Live) is the paging row in this fixture.
    let mut s = s;
    s.home.rows[2].paging = Paging::Loading("c".into());
    let failed = Event::MoreLoaded {
        feed: FeedId::Home,
        kind: RowKind::TopLive,
        cursor: "c".into(),
        outcome: PageOutcome::Failed,
    };
    insta::assert_snapshot!(render(&view_model(&reduce(&s, failed).0)));
}

#[test]
fn home_failed_to_load() {
    let (s, _) = init();
    let failed = Event::FeedFailed {
        feed: FeedId::Home,
        message: "network error: timed out".into(),
    };
    insta::assert_snapshot!(render(&view_model(&reduce(&s, failed).0)));
}

fn browse() -> AppState {
    let (s, _) = reduce(&home_loaded(), Intent::RailPick(RailEntry::Browse));
    let tiles: Vec<Tile> = (0..7)
        .map(|i| category(&format!("g{i}"), &format!("Game {i}")))
        .collect();
    let rows = grid_rows(RowKind::Categories, tiles, Paging::More("next".into()));
    reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Browse,
            rows,
        },
    )
    .0
}

#[test]
fn browse_directory() {
    insta::assert_snapshot!(render(&view_model(&browse())));
}

#[test]
fn category_streams_while_loading_and_loaded() {
    let (s, _) = reduce(&browse(), Intent::Select);
    let loading = render(&view_model(&s));
    let rows = grid_rows(
        RowKind::CategoryStreams,
        (0..4)
            .map(|i| stream(&format!("c{i}"), 500 * (i + 1), i != 1))
            .collect(),
        Paging::End,
    );
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Category("g0".into()),
            rows,
        },
    )
    .0;
    insta::assert_snapshot!(format!("{loading}---\n{}", render(&view_model(&s))));
}

fn player_line(vm: &ViewModel) -> String {
    format!(
        "screen={:?} playback={:?} error={:?} paused={} select={:?} retry={}\n",
        vm.screen, vm.playback, vm.playback_error, vm.paused, vm.select_label, vm.can_retry
    )
}

#[test]
fn player_loading_playing_paused_offline_and_failed() {
    let (s, _) = reduce(&home_loaded(), Intent::Select);
    let mut out = player_line(&view_model(&s));
    let s = reduce(&s, Event::PlaybackStarted).0;
    out += &player_line(&view_model(&s));
    let paused = reduce(&s, Intent::Select).0;
    out += &player_line(&view_model(&paused));
    let ended = reduce(&s, Event::PlaybackEnded).0;
    out += &player_line(&view_model(&ended));
    let (loading, _) = reduce(&home_loaded(), Intent::Select);
    let failed = reduce(
        &loading,
        Event::PlaybackFailed {
            channel: "grandmaster_k".into(),
            message: "Streamlink is not installed".into(),
        },
    )
    .0;
    out += &player_line(&view_model(&failed));
    insta::assert_snapshot!(out);
}

fn menu_lines(vm: &ViewModel) -> String {
    let mut out = format!(
        "menu_open={} focus={} select={:?} values={:?}\n",
        vm.player_menu_open, vm.player_menu_focus, vm.select_label, vm.player_menu_values
    );
    if !vm.stats_text.is_empty() {
        for line in vm.stats_text.lines() {
            out += &format!("  stats| {line}\n");
        }
    }
    out
}

#[test]
fn player_menu_and_stats_box() {
    let (s, _) = reduce(&home_loaded(), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted).0;
    let mut out = menu_lines(&view_model(&s));
    // Start, then down to Zoom and fill the screen.
    let s = send(&s, &[Intent::Menu]);
    out += &menu_lines(&view_model(&s));
    let s = send(
        &s,
        &[Intent::Down, Intent::Down, Intent::Down, Intent::Right],
    );
    out += &menu_lines(&view_model(&s));
    // Stats on: waiting for the first report, then the numbers.
    let s = send(&s, &[Intent::Down, Intent::Right]);
    out += &menu_lines(&view_model(&s));
    let report = PlayerStats {
        width: Some(1920),
        height: Some(1080),
        fps_milli: Some(59_940),
        codec: Some("h264".into()),
        hwdec: Some("vaapi".into()),
        video_bitrate: Some(6_240_000),
        audio_bitrate: Some(160_000),
        audio_codec: Some("aac".into()),
        buffer_ms: Some(1_450),
        dropped_frames: Some(3),
        decoder_dropped_frames: Some(0),
    };
    let s = reduce(&s, Event::PlayerStats(report)).0;
    out += &menu_lines(&view_model(&s));
    // B closes the menu, and the stats box stays.
    let s = send(&s, &[Intent::Back]);
    out += &menu_lines(&view_model(&s));
    // A report that knows nothing yet (software decoding, nothing measured).
    let s = reduce(&s, Event::PlayerStats(PlayerStats::default())).0;
    out += &menu_lines(&view_model(&s));
    insta::assert_snapshot!(out);
}

#[test]
fn player_audio_only() {
    let (s, _) = reduce(&home_loaded(), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted).0;
    // Stats on, then down to Audio only and turn it on.
    let s = send(&s, &[Intent::Menu, Intent::Down, Intent::Down]);
    let s = send(
        &s,
        &[Intent::Down, Intent::Down, Intent::Right, Intent::Down],
    );
    let s = send(&s, &[Intent::Right]);
    let mut out = format!("audio_only={}\n", view_model(&s).audio_only);
    out += &menu_lines(&view_model(&s));
    let s = reduce(&s, Event::PlaybackStarted).0;
    let report = PlayerStats {
        audio_bitrate: Some(160_000),
        audio_codec: Some("aac".into()),
        buffer_ms: Some(2_000),
        ..PlayerStats::default()
    };
    let s = reduce(&s, Event::PlayerStats(report)).0;
    out += &menu_lines(&view_model(&s));
    // Up to the locked Quality row: A changes nothing.
    let s = send(&s, &[Intent::Up; 5]);
    let s = send(&s, &[Intent::Select]);
    out += &menu_lines(&view_model(&s));
    // Leaving the player turns it off.
    let s = send(&s, &[Intent::Back, Intent::Back]);
    out += &format!("after leaving: audio_only={}\n", view_model(&s).audio_only);
    insta::assert_snapshot!(out);
}

fn sleep_lines(vm: &ViewModel) -> String {
    format!(
        "screen={:?} sleep_label={:?} warning={} menu_value={:?}\n",
        vm.screen,
        vm.sleep_label,
        vm.sleep_warning,
        vm.player_menu_values
            .get(PlayerMenuRow::SleepTimer.index())
            .map(String::as_str)
            .unwrap_or("")
    )
}

fn subscribe_lines(vm: &ViewModel) -> String {
    format!(
        "menu_open={} focus={} select={:?} card={} url={:?} qr={}x{} can_open={}\n",
        vm.player_menu_open,
        vm.player_menu_focus,
        vm.select_label,
        vm.subscribe_open,
        vm.subscribe_url,
        vm.subscribe_qr_width,
        vm.subscribe_qr_width,
        vm.subscribe_can_open
    )
}

#[test]
fn subscribe_card_with_and_without_a_browser() {
    let mut out = String::new();
    for can_open_browser in [true, false] {
        let mut s = home_loaded();
        s.can_open_browser = can_open_browser;
        out += &format!("can_open_browser={can_open_browser}\n");
        let (s, _) = reduce(&s, Intent::Select);
        let s = reduce(&s, Event::PlaybackStarted).0;
        // Start, down to the last row.
        let s = send(&s, &[Intent::Menu]);
        let s = send(&s, &[Intent::Down; 7]);
        out += &subscribe_lines(&view_model(&s));
        // A opens the card, B closes it again.
        let s = send(&s, &[Intent::Select]);
        out += &subscribe_lines(&view_model(&s));
        let s = send(&s, &[Intent::Back]);
        out += &subscribe_lines(&view_model(&s));
    }
    insta::assert_snapshot!(out);
}

#[test]
fn sleep_timer_countdown_and_warning() {
    let (s, _) = reduce(&home_loaded(), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted).0;
    let mut out = sleep_lines(&view_model(&s));
    // Start, down to the sleep timer, 15 then 30 minutes.
    let s = send(&s, &[Intent::Menu]);
    let s = send(&s, &[Intent::Down; 6]);
    let s = send(&s, &[Intent::Right, Intent::Right]);
    out += &sleep_lines(&view_model(&s));
    let id = s.sleep.unwrap().id;
    let s = reduce(&s, Event::SleepTimerTick { id, left_min: 2 }).0;
    out += &sleep_lines(&view_model(&s));
    let s = send(&s, &[Intent::Back]);
    let s = reduce(&s, Event::SleepTimerWarning { id }).0;
    out += &sleep_lines(&view_model(&s));
    // A button keeps going: a fresh 30 minutes.
    let s = send(&s, &[Intent::Select]);
    out += &sleep_lines(&view_model(&s));
    // Left on the browse screens, the warning still shows.
    let s = send(&s, &[Intent::Back]);
    let id = s.sleep.unwrap().id;
    let s = reduce(&s, Event::SleepTimerWarning { id }).0;
    out += &sleep_lines(&view_model(&s));
    insta::assert_snapshot!(out);
}

fn chat_lines(vm: &ViewModel) -> String {
    let mut out = format!(
        "open={} overlay={} status={:?}\n",
        vm.chat_open, vm.chat_overlay, vm.chat_status
    );
    for line in &vm.chat {
        let pieces: Vec<String> = line
            .pieces
            .iter()
            .map(|p| {
                if p.emote {
                    format!("<{}>", p.text)
                } else {
                    p.text.clone()
                }
            })
            .collect();
        out += &format!(
            "{:?} #{:02x}{:02x}{:02x} badges={} {}: {}\n",
            line.kind,
            line.tint.r,
            line.tint.g,
            line.tint.b,
            line.badges.len(),
            line.user,
            pieces.join(" ")
        );
    }
    out
}

#[test]
fn chat_connecting_live_busy_moderated_and_reconnecting() {
    let channel = "grandmaster_k";
    let (s, _) = reduce(&home_loaded(), Intent::Select);
    let mut out = chat_lines(&view_model(&s));

    let live = Event::ChatStatus {
        channel: channel.into(),
        status: ChatStatus::Live,
    };
    let s = reduce(&s, live).0;
    out += &chat_lines(&view_model(&s));

    let mod_line = ChatMessage {
        id: "1".into(),
        color: Some(Rgb::new(30, 144, 255)),
        badges: vec!["https://cdn/mod.png".into(), "https://cdn/sub.png".into()],
        spans: vec![
            ChatSpan::Text("nice play".into()),
            ChatSpan::Emote {
                name: "Kappa".into(),
                url: "https://cdn/kappa.png".into(),
            },
        ],
        ..ChatMessage::plain("ModMarie", "")
    };
    let troll = ChatMessage {
        id: "2".into(),
        // A near-black name is lightened.
        color: Some(Rgb::new(0, 0, 40)),
        ..ChatMessage::plain("Troll", "buy followers")
    };
    let me = ChatMessage {
        kind: ChatKind::Action,
        color: Some(Rgb::new(255, 105, 180)),
        ..ChatMessage::plain("Waver", "waves at chat")
    };
    let batch = Event::ChatMessages {
        channel: channel.into(),
        messages: vec![
            mod_line,
            troll,
            me,
            ChatMessage::notice("This room is now in slow mode."),
            ChatMessage::plain("plain_pat", "no colour set"),
        ],
    };
    let s = reduce(&s, batch).0;
    out += &chat_lines(&view_model(&s));

    let ban = Event::ChatModerated {
        channel: channel.into(),
        action: Moderation::ClearUser("troll".into()),
    };
    let s = reduce(&s, ban).0;
    out += &chat_lines(&view_model(&s));

    let dropped = Event::ChatStatus {
        channel: channel.into(),
        status: ChatStatus::Reconnecting,
    };
    let mut s = reduce(&s, dropped).0;
    s.chat_open = true;
    s.settings.chat_layout = ChatLayout::Overlay;
    out += &chat_lines(&view_model(&s));
    insta::assert_snapshot!(out);
}
