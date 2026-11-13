//! `insta` snapshots of the view-model for each Browse state. The view-model is rendered as plain
//! text (one line per card) so a diff in review shows exactly which card changed.
//!
//! Review changes with `cargo insta review`, or accept them with `INSTA_UPDATE=always cargo test`.

use htpc_core::{
    grid_rows, init, reduce, AppState, Category, ChannelInfo, ChannelRef, ChatKind, ChatLayout,
    ChatMessage, ChatSpan, ChatStatus, Clip, Event, FeedId, Intent, Moderation, PageOutcome,
    Paging, PlayerMenuRow, PlayerStats, Progress, RailEntry, Rgb, Row, RowId, RowKind, Span,
    Stream, Tile, Video, VideoKind,
};
use htpc_ui::{format_ago, format_date, view_model, CardVm, ViewModel};

fn stream(channel: &str, viewers: u64, image: bool) -> Tile {
    Tile::Stream(Stream {
        channel: ChannelRef::twitch(channel),
        user_id: String::new(),
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
    if c.picked {
        out += " | picked";
    }
    // Video and clip cards (the channel page).
    if !c.length.is_empty() {
        out += &format!(" | {} | {}", c.length, c.meta);
        if !c.title.is_empty() {
            out += &format!(" | {}", c.title);
        }
        if c.progress > 0 {
            out += &format!(" | progress {}/1000", c.progress);
        }
        if c.muted_audio {
            out += " | some audio muted";
        }
        if c.locked {
            out += " | locked";
        }
    }
    if c.resume {
        let images = c.mosaic.iter().filter(|m| !m.thumb_url.is_empty()).count();
        out += &format!(
            " | {} | mosaic of {}, {images} with img",
            c.title,
            c.mosaic.len()
        );
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
        row: RowId::twitch(RowKind::TopLive),
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
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    out += &player_line(&view_model(&s));
    let paused = reduce(&s, Intent::Select).0;
    out += &player_line(&view_model(&paused));
    let ended = reduce(&s, Event::PlaybackEnded { slot: 0 }).0;
    out += &player_line(&view_model(&ended));
    let (loading, _) = reduce(&home_loaded(), Intent::Select);
    let failed = reduce(
        &loading,
        Event::PlaybackFailed {
            channel: ChannelRef::twitch("grandmaster_k"),
            message: "Streamlink is not installed".into(),
        },
    )
    .0;
    out += &player_line(&view_model(&failed));
    insta::assert_snapshot!(out);
}

fn end_card_lines(vm: &ViewModel) -> String {
    let mut out = format!(
        "screen={:?} playback={:?} select={:?} retry={} raid_open={}\n",
        vm.screen, vm.playback, vm.select_label, vm.can_retry, vm.raid_open
    );
    for (i, row) in vm.live_end_rows.iter().enumerate() {
        let mark = if i as i32 == vm.live_end_focus {
            ">"
        } else {
            " "
        };
        out += &format!("  {mark} {row}\n");
    }
    out
}

/// The live end card (vod-plan.md, phase 7): a stream that ended, its broadcast known, a raid
/// over it turned down; and the *You were away* toast after a wake while it was still live.
#[test]
fn live_end_card_and_the_away_toast() {
    let user = htpc_core::User {
        login: "viewer".into(),
        can_chat: true,
    };
    let s = reduce(&home_loaded(), Event::SignedIn(user)).0;
    let (s, _) = reduce(&s, Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    let archive = |checked_at| Event::LiveArchive {
        channel: ChannelRef::twitch("grandmaster_k"),
        archive: Some(htpc_core::LiveArchive {
            video_id: "500".into(),
            started_at: 1_794_300_000,
            checked_at,
        }),
    };
    let s = reduce(&s, archive(1_794_300_600)).0;
    let playing = s.clone();

    let mut out = String::from("-- ended, the broadcast not known\n");
    let (unknown, _) = reduce(&home_loaded(), Intent::Select);
    let unknown = reduce(&unknown, Event::PlaybackStarted { slot: 0 }).0;
    let unknown = reduce(&unknown, Event::PlaybackEnded { slot: 0 }).0;
    out += &end_card_lines(&view_model(&unknown));

    out += "-- ended, the broadcast known\n";
    let s = reduce(&s, Event::PlaybackEnded { slot: 0 }).0;
    out += &end_card_lines(&view_model(&s));
    out += "-- down\n";
    let s = send(&s, &[Intent::Down]);
    out += &end_card_lines(&view_model(&s));
    out += "-- a raid: its card goes first\n";
    let s = reduce(
        &s,
        Event::Raided {
            from_login: "grandmaster_k".into(),
            to_login: "next_up".into(),
            to_name: "Next_Up".into(),
            viewers: 300,
        },
    )
    .0;
    out += &end_card_lines(&view_model(&s));
    out += "-- turned down: the first row\n";
    let s = send(&s, &[Intent::Back]);
    out += &end_card_lines(&view_model(&s));

    out += "-- asleep 2 h 10 min, still live\n";
    let s = reduce(
        &playing,
        Event::DeviceWoke {
            asleep_ms: 7_800_000,
            before_s: 1_794_301_200,
        },
    )
    .0;
    let s = reduce(&s, archive(1_794_309_000)).0;
    out += &toast_lines(&view_model(&s));
    out += "-- asleep, and it ended in the sleep\n";
    let s = reduce(&s, Event::PlaybackEnded { slot: 0 }).0;
    out += &end_card_lines(&view_model(&s));
    insta::assert_snapshot!(out);
}

#[test]
fn time_away_reads_to_the_minute() {
    assert_eq!(htpc_ui::format_away(10_000), "1 min");
    assert_eq!(htpc_ui::format_away(45 * 60_000), "45 min");
    assert_eq!(htpc_ui::format_away(2 * 3_600_000), "2 h");
    assert_eq!(htpc_ui::format_away(7_800_000), "2 h 10 min");
    assert_eq!(htpc_ui::format_away(7_829_000), "2 h 10 min");
}

fn menu_lines(vm: &ViewModel) -> String {
    let values: Vec<&String> = vm.player_menu_rows.iter().map(|r| &r.value).collect();
    let mut out = format!(
        "menu_open={} focus={} select={:?} values={:?}\n",
        vm.player_menu_open, vm.player_menu_focus, vm.select_label, values
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
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
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
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    // Stats on, then down to Audio only and turn it on.
    let s = send(&s, &[Intent::Menu, Intent::Down, Intent::Down]);
    let s = send(
        &s,
        &[Intent::Down, Intent::Down, Intent::Right, Intent::Down],
    );
    let s = send(&s, &[Intent::Right]);
    let mut out = format!("audio_only={}\n", view_model(&s).audio_only);
    out += &menu_lines(&view_model(&s));
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
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
        vm.player_menu_rows
            .iter()
            .find(|r| r.title == "Sleep timer")
            .map(|r| r.value.as_str())
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
        let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
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
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
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

fn raid_lines(vm: &ViewModel) -> String {
    format!(
        "screen={:?} channel={:?} playback={:?} raid_open={} title={:?} viewers={:?} seconds={} select={:?} retry={}\n",
        vm.screen,
        vm.now_playing.channel,
        vm.playback,
        vm.raid_open,
        vm.raid_title,
        vm.raid_viewers,
        vm.raid_seconds,
        vm.select_label,
        vm.can_retry
    )
}

#[test]
fn raid_card_counts_down_then_follows() {
    let user = htpc_core::User {
        login: "viewer".into(),
        can_chat: true,
    };
    let s = reduce(&home_loaded(), Event::SignedIn(user)).0;
    let (s, _) = reduce(&s, Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    let raid = |viewers| Event::Raided {
        from_login: "grandmaster_k".into(),
        to_login: "next_up".into(),
        to_name: "Next_Up".into(),
        viewers,
    };
    let mut out = String::from("-- playing\n");
    out += &raid_lines(&view_model(&s));
    let s = reduce(&s, raid(48_100)).0;
    out += "-- raided\n";
    out += &raid_lines(&view_model(&s));
    let id = s.raid.as_ref().unwrap().id;
    let s = reduce(&s, Event::RaidTick { id, left: 3 }).0;
    out += "-- three seconds left\n";
    out += &raid_lines(&view_model(&s));
    out += "-- one viewer, none\n";
    for viewers in [1, 0] {
        let (stay, _) = reduce(&s, Intent::Back);
        out += &raid_lines(&view_model(&reduce(&stay, raid(viewers)).0));
    }
    let s = reduce(&s, Event::RaidCountdownDone { id }).0;
    out += "-- followed\n";
    out += &raid_lines(&view_model(&s));
    insta::assert_snapshot!(out);
}

fn hype_lines(vm: &ViewModel) -> String {
    format!(
        "screen={:?} channel={:?} hype_open={} level={} permille={} points={:?} seconds={}\n",
        vm.screen,
        vm.now_playing.channel,
        vm.hype_open,
        vm.hype_level,
        vm.hype_permille,
        vm.hype_points,
        vm.hype_seconds,
    )
}

#[test]
fn hype_train_bar_follows_the_polls() {
    let (s, _) = reduce(&home_loaded(), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    let channel = s.now_playing.as_ref().unwrap().channel.login.clone();
    let hype = |level, progress, goal, seconds_left| Event::HypeTrain {
        channel: channel.clone(),
        train: Some(htpc_core::HypeTrain {
            level,
            progress,
            goal,
            seconds_left,
        }),
    };
    let mut out = String::from("-- playing\n");
    out += &hype_lines(&view_model(&s));
    let s = reduce(&s, hype(1, 1_501, 1_600, 211)).0;
    out += "-- level 1\n";
    out += &hype_lines(&view_model(&s));
    let s = reduce(&s, hype(2, 401, 1_800, 297)).0;
    out += "-- level up\n";
    out += &hype_lines(&view_model(&s));
    let s = reduce(&s, hype(2, 2_000, 1_800, 12)).0;
    out += "-- overshoot is drawn full\n";
    out += &hype_lines(&view_model(&s));
    let ended = reduce(
        &s,
        Event::HypeTrain {
            channel: channel.clone(),
            train: None,
        },
    )
    .0;
    out += "-- ended\n";
    out += &hype_lines(&view_model(&ended));
    let (left, _) = reduce(&s, Intent::Back);
    out += "-- left the player\n";
    out += &hype_lines(&view_model(&left));
    insta::assert_snapshot!(out);
}

fn search_lines(vm: &ViewModel) -> String {
    let mut out = format!(
        "query={:?} key={} on_keys={} in_results={} select={:?} retry={} status={:?}\n",
        vm.search_query,
        vm.search_key_focus,
        vm.search_on_keys,
        vm.search_in_results,
        vm.select_label,
        vm.can_retry,
        vm.search_status
    );
    out += &render(vm);
    out
}

#[test]
fn search_empty_typing_and_results() {
    let user = htpc_core::User {
        login: "nightowl_dev".into(),
        can_chat: true,
    };
    let s = reduce(&home_loaded(), Event::SignedIn(user)).0;
    let s = send(&s, &[Intent::RailPick(RailEntry::Search)]);
    let mut out = String::from("-- empty\n");
    out += &search_lines(&view_model(&s));

    // `l`, `o` typed on the keyboard (down a row, right to `o`), then waiting for the answer.
    let s = send(
        &s,
        &[
            Intent::Down,
            Intent::Right,
            Intent::Select,
            Intent::Right,
            Intent::Right,
            Intent::Right,
            Intent::Select,
        ],
    );
    out += "-- typing\n";
    out += &search_lines(&view_model(&s));

    let id = s.search.id;
    let rows = vec![
        Row::new(
            RowKind::SearchLive,
            vec![
                stream("lofi_lena", 860, true),
                stream("lorekeeper", 42, false),
            ],
        ),
        Row::new(RowKind::SearchCategories, vec![category("27471", "Lords")]),
        Row::new(
            RowKind::SearchOffline,
            vec![Tile::Channel(htpc_core::Channel {
                channel: ChannelRef::twitch("lonely_owl"),
                name: "Lonely_Owl".into(),
                tint: Rgb::from_hex(0x2d4a6b),
            })],
        ),
    ];
    let s = reduce(&s, Event::SearchResults { id, rows }).0;
    out += "-- results, keyboard focused\n";
    out += &search_lines(&view_model(&s));
    let s = send(&s, &[Intent::Menu, Intent::Down]);
    out += "-- results focused\n";
    out += &search_lines(&view_model(&s));

    // Nothing found, and a failed search.
    let s = send(&s, &[Intent::Back, Intent::Text('x')]);
    let id = s.search.id;
    let none = reduce(
        &s,
        Event::SearchResults {
            id,
            rows: Vec::new(),
        },
    )
    .0;
    out += "-- nothing found\n";
    out += &search_lines(&view_model(&none));
    let failed = reduce(
        &s,
        Event::SearchFailed {
            id,
            message: "network error: timed out".into(),
        },
    )
    .0;
    out += "-- failed\n";
    out += &search_lines(&view_model(&failed));
    insta::assert_snapshot!(out);
}

fn toast_lines(vm: &ViewModel) -> String {
    let mut out = format!("screen={:?} more={}\n", vm.screen, vm.toast_more);
    for t in &vm.toasts {
        if t.note {
            out += &format!("  [#{} note: {}]\n", t.id, t.name);
            continue;
        }
        let img = if t.thumb_url.is_empty() { "" } else { " | img" };
        out += &format!("  [#{} {} is live | {}{img}]\n", t.id, t.name, t.line);
    }
    out
}

#[test]
fn the_resume_card_on_home_and_the_skipped_toast() {
    let (mut s, _) = init();
    s.user = Some(htpc_core::User {
        login: "viewer".into(),
        can_chat: true,
    });
    s.last_multiview = Some(htpc_core::SavedSet {
        channels: ["lofi_lena", "quietcoast", "marrow", "sable", "sleepy"]
            .into_iter()
            .map(ChannelRef::twitch)
            .collect(),
        layout: htpc_core::Layout::Grid(6),
        sound: 0,
    });
    let offline = Tile::Channel(htpc_core::Channel {
        channel: ChannelRef::twitch("sleepy"),
        name: "sleepy".into(),
        tint: Rgb::default(),
    });
    // What the provider found: four live, one offline.
    let checked = Row::new(
        RowKind::Resume,
        vec![
            stream("lofi_lena", 100, true),
            stream("quietcoast", 100, false),
            stream("marrow", 100, true),
            stream("sable", 100, true),
            offline,
        ],
    );
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![
                Row::new(
                    RowKind::Featured,
                    vec![stream("grandmaster_k", 48_100, true)],
                ),
                Row::new(RowKind::TopLive, vec![stream("pixelpilgrim", 900, true)]),
                checked,
            ],
        },
    )
    .0;
    let mut out = String::from("-- the card opens Top Live\n");
    let s = send(&s, &[Intent::Down]);
    out += &render(&view_model(&s));
    out += "-- A: the four start, the offline one is skipped\n";
    let s = send(&s, &[Intent::Select]);
    out += &toast_lines(&view_model(&s));
    insta::assert_snapshot!(out);
}

#[test]
fn live_notification_toasts_one_three_and_more() {
    let user = htpc_core::User {
        login: "nightowl_dev".into(),
        can_chat: true,
    };
    let s = reduce(&home_loaded(), Event::SignedIn(user)).0;
    let went_live = |s: &AppState, channels: &[&str]| {
        let streams = channels
            .iter()
            .map(|c| match stream(c, 100, *c != "quietcoast") {
                Tile::Stream(s) => s,
                _ => unreachable!(),
            })
            .collect();
        reduce(s, Event::WentLive(streams)).0
    };
    let mut out = String::from("-- one\n");
    let s = went_live(&s, &["lofi_lena"]);
    out += &toast_lines(&view_model(&s));
    out += "-- three, over the player\n";
    let s = send(&s, &[Intent::Select]);
    let s = went_live(&s, &["marrow", "quietcoast"]);
    out += &toast_lines(&view_model(&s));
    out += "-- five\n";
    let s = went_live(&s, &["pixelpilgrim", "sable"]);
    out += &toast_lines(&view_model(&s));
    out += "-- the oldest expired\n";
    let s = reduce(&s, Event::ToastExpired(1)).0;
    out += &toast_lines(&view_model(&s));
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
        channel: ChannelRef::twitch(channel),
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
        channel: ChannelRef::twitch(channel),
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
        channel: ChannelRef::twitch(channel),
        action: Moderation::ClearUser("troll".into()),
    };
    let s = reduce(&s, ban).0;
    out += &chat_lines(&view_model(&s));

    let dropped = Event::ChatStatus {
        channel: ChannelRef::twitch(channel),
        status: ChatStatus::Reconnecting,
    };
    let mut s = reduce(&s, dropped).0;
    s.chat_open = true;
    s.settings.chat_layout = ChatLayout::Overlay;
    out += &chat_lines(&view_model(&s));
    insta::assert_snapshot!(out);
}

// --- Multiview ---------------------------------------------------------------------------------

/// The followed channels the picker lists, in order.
const FOLLOWED: [&str; 9] = [
    "ash", "birch", "cedar", "dune", "elm", "fern", "gale", "heath", "isle",
];

/// Signed in on Home, Following loaded with the live `FOLLOWED` (the fourth one on Kick, no
/// viewers known), watching `grandmaster_k` in the single player.
fn watching_signed_in() -> AppState {
    let user = htpc_core::User {
        login: "viewer".into(),
        can_chat: true,
    };
    let s = reduce(&home_loaded(), Event::SignedIn(user)).0;
    let followed: Vec<Tile> = FOLLOWED
        .iter()
        .enumerate()
        .map(|(i, login)| {
            let mut tile = stream(login, 1_000 * (i as u64 + 1), i % 2 == 0);
            if let Tile::Stream(s) = &mut tile {
                if i == 3 {
                    s.channel = ChannelRef::kick(*login);
                    s.viewers = 0;
                }
            }
            tile
        })
        .collect();
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Following,
            rows: vec![Row::new(RowKind::FollowedLive, followed)],
        },
    )
    .0;
    let (s, _) = reduce(&s, Intent::Select);
    reduce(&s, Event::PlaybackStarted { slot: 0 }).0
}

/// Multiview with `grandmaster_k` and the first `n - 1` followed channels, settled and playing,
/// the picker closed and the focus on the second stream.
fn multiview_with(n: usize) -> AppState {
    let mut s = send(&watching_signed_in(), &[Intent::View, Intent::Select]);
    for i in 0..n - 1 {
        if i > 0 {
            s = reduce(&s, Intent::Down).0;
        }
        s = reduce(&s, Intent::Select).0;
    }
    if s.multiview.as_ref().unwrap().picker.is_some() {
        s = reduce(&s, Intent::Back).0;
    }
    s = reduce(&s, Event::QualitySettleDue(s.last_settle_id)).0;
    let slots: Vec<u8> = s
        .multiview
        .as_ref()
        .unwrap()
        .tiles
        .iter()
        .flatten()
        .map(|t| t.slot)
        .collect();
    for slot in slots {
        s = reduce(&s, Event::PlaybackStarted { slot }).0;
    }
    s.multiview.as_mut().unwrap().focus = 1;
    s
}

fn mv_lines(vm: &ViewModel) -> String {
    let mv = &vm.mv;
    let mut out = format!(
        "screen={:?} active={} count={:?} select={:?} retry={} video_slot={} chat_open={}\n",
        vm.screen,
        mv.active,
        mv.count_text,
        vm.select_label,
        vm.can_retry,
        vm.video_slot,
        vm.chat_open
    );
    let chips: Vec<String> = mv
        .layouts
        .iter()
        .map(|c| {
            let mark = if c.on {
                "*"
            } else if c.fits {
                ""
            } else {
                "~"
            };
            format!("[{}{mark}]", c.label)
        })
        .collect();
    out += &format!("  layouts {}\n", chips.join(" "));
    for (i, t) in mv.tiles.iter().enumerate() {
        out += &format!("  {i}: {:.1},{:.1} {:.1}x{:.1}", t.x, t.y, t.w, t.h);
        if t.empty {
            out += " [add a stream]";
        } else {
            out += &format!(
                " slot{} {} {:?} {}{}{}{} {:?}",
                t.slot,
                t.channel,
                t.state,
                t.quality,
                if t.lowered { " ↓amber" } else { "" },
                if t.sound { " SOUND" } else { "" },
                if t.kick { " kick" } else { "" },
                t.line
            );
        }
        if t.focused {
            out += &format!(" <focus, A {:?}>", t.action);
        }
        out += "\n";
    }
    out
}

#[test]
fn multiview_grids_of_two_four_and_nine() {
    let mut out = String::new();
    for n in [2, 4, 9] {
        out += &format!("-- {n} streams\n");
        out += &mv_lines(&view_model(&multiview_with(n)));
    }
    insta::assert_snapshot!(out);
}

#[test]
fn multiview_slow_network_lowers_the_muted_tiles() {
    let s = multiview_with(4);
    let stall = htpc_core::Health {
        stalled: true,
        buffer_ms: 0,
        ..Default::default()
    };
    let s = reduce(
        &s,
        Event::PlayerHealth {
            slot: 1,
            at_ms: 0,
            health: stall,
        },
    )
    .0;
    let vm = view_model(&s);
    let out = format!("{}{}", mv_lines(&vm), toast_lines(&vm));
    insta::assert_snapshot!(out);
}

#[test]
fn multiview_spotlight_with_three_and_seven() {
    let mut out = String::new();
    // From the grid of 3, LB wraps back to Spotlight; from the grid of 9, RB goes there.
    for (n, step) in [(3, Intent::PageLeft), (7, Intent::PageRight)] {
        let s = send(&multiview_with(n), &[step]);
        out += &format!("-- {n} streams\n");
        out += &mv_lines(&view_model(&s));
        // The sound moves to the focused small tile, which becomes the big one.
        let s = send(&s, &[Intent::Select]);
        out += "   sound moved\n";
        out += &mv_lines(&view_model(&s));
    }
    insta::assert_snapshot!(out);
}

fn popup_lines(vm: &ViewModel) -> String {
    let mv = &vm.mv;
    let mut out = format!(
        "open={} title={:?} sub={:?} on_layout={} select={:?}\n",
        mv.popup_open, mv.popup_title, mv.popup_sub, mv.popup_on_layout, vm.select_label
    );
    for (i, r) in mv.popup_rows.iter().enumerate() {
        let mark = if i as i32 == mv.popup_focus { ">" } else { " " };
        out += &format!("  {mark} {:?} {:?}\n", r.label, r.value);
    }
    out
}

#[test]
fn multiview_view_popup() {
    let mut out = String::from("-- single player, signed out\n");
    let (s, _) = reduce(&home_loaded(), Intent::Select);
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    out += &popup_lines(&view_model(&send(&s, &[Intent::View])));

    out += "-- single player, signed in\n";
    out += &popup_lines(&view_model(&send(&watching_signed_in(), &[Intent::View])));

    out += "-- grid, on a muted tile\n";
    let grid = multiview_with(3);
    let s = send(&grid, &[Intent::View]);
    out += &popup_lines(&view_model(&s));
    out += &mv_lines(&view_model(&s));

    out += "-- grid, the layout row\n";
    let s = send(&s, &[Intent::Down, Intent::Down, Intent::Down]);
    out += &popup_lines(&view_model(&s));
    let s = send(&s, &[Intent::Right]);
    out += &popup_lines(&view_model(&s));

    out += "-- full screen\n";
    let full = send(&grid, &[Intent::Up, Intent::Left]);
    let full = send(&full, &[Intent::Select]);
    out += &popup_lines(&view_model(&send(&full, &[Intent::View])));

    out += "-- A on an offline tile\n";
    let offline = reduce(
        &grid,
        Event::PlaybackEnded {
            slot: grid.multiview.as_ref().unwrap().tile(1).unwrap().slot,
        },
    )
    .0;
    out += &mv_lines(&view_model(&offline));
    out += &popup_lines(&view_model(&send(&offline, &[Intent::Select])));
    insta::assert_snapshot!(out);
}

#[test]
fn multiview_picking_on_browse_cards() {
    let user = htpc_core::User {
        login: "viewer".into(),
        can_chat: true,
    };
    let home = reduce(&home_loaded(), Event::SignedIn(user)).0;
    let browse = |vm: &ViewModel| {
        format!(
            "can_view={} tray={:?}\n{}",
            vm.can_view,
            vm.tray_text,
            render(vm)
        )
    };
    let mut out = String::from("-- nothing picked, on the hero\n");
    out += &browse(&view_model(&home));
    out += &popup_lines(&view_model(&send(&home, &[Intent::View])));

    out += "-- the hero picked, on the next card\n";
    let s = send(&home, &[Intent::View, Intent::Select, Intent::Down]);
    out += &browse(&view_model(&s));
    out += &popup_lines(&view_model(&send(&s, &[Intent::View])));

    out += "-- back on the hero\n";
    out += &popup_lines(&view_model(&send(&s, &[Intent::Up, Intent::View])));

    out += "-- a category card\n";
    let cat = send(&s, &[Intent::Down, Intent::Down]);
    out += &browse(&view_model(&cat));

    out += "-- the single player, with the hero picked\n";
    let player = send(&s, &[Intent::Select]);
    let player = reduce(&player, Event::PlaybackStarted { slot: 0 }).0;
    out += &popup_lines(&view_model(&send(&player, &[Intent::View])));

    out += "-- the two watched together\n";
    let mv = send(&s, &[Intent::View, Intent::Down, Intent::Select]);
    out += &mv_lines(&view_model(&mv));

    out += "-- B: back on Home, both still picked\n";
    out += &browse(&view_model(&send(&mv, &[Intent::Back])));
    insta::assert_snapshot!(out);
}

fn picker_lines(vm: &ViewModel) -> String {
    let mv = &vm.mv;
    let mut out = format!(
        "open={} tab={} focus={} select={:?} empty={:?}\n",
        mv.picker_open, mv.picker_tab, mv.picker_focus, vm.select_label, mv.picker_empty
    );
    for (i, (row, watching)) in mv.picker_rows.iter().zip(&mv.picker_watching).enumerate() {
        let mark = if i as i32 == mv.picker_focus {
            ">"
        } else {
            " "
        };
        let tag = if *watching { " (watching)" } else { "" };
        out += &format!("  {mark} {}{tag}\n", card(row));
    }
    out
}

#[test]
fn multiview_picker() {
    let s = send(&watching_signed_in(), &[Intent::View, Intent::Select]);
    let mut out = String::from("-- opened from the single player\n");
    out += &picker_lines(&view_model(&s));
    out += &mv_lines(&view_model(&s));

    out += "-- after adding the first, on the second\n";
    let s = send(&s, &[Intent::Select, Intent::Down]);
    out += &picker_lines(&view_model(&s));

    out += "-- back on the one already added\n";
    out += &picker_lines(&view_model(&send(&s, &[Intent::Up])));

    out += "-- Favorites (none yet)\n";
    out += &picker_lines(&view_model(&send(&s, &[Intent::PageRight])));

    out += "-- Following still loading, with nothing yet\n";
    let mut loading = send(&s, &[Intent::Back]);
    loading.following.rows.clear();
    // View, *Add a stream*: the picker asks Following again.
    let loading = send(&loading, &[Intent::View, Intent::Select]);
    out += &picker_lines(&view_model(&loading));
    insta::assert_snapshot!(out);
}

fn favorite_picker_lines(vm: &ViewModel) -> String {
    let mv = &vm.mv;
    let mut out = format!(
        "tab={} focus={} select={:?} y={:?} empty={:?}\n",
        mv.picker_tab, mv.picker_focus, vm.select_label, mv.picker_star_label, mv.picker_empty
    );
    for (i, row) in mv.picker_rows.iter().enumerate() {
        let mark = if i as i32 == mv.picker_focus {
            ">"
        } else {
            " "
        };
        let star = if mv.picker_starred[i] { "★ " } else { "" };
        let off = if row.offline { " (offline)" } else { "" };
        out += &format!("  {mark} {star}{}{off}\n", card(row));
    }
    out
}

#[test]
fn favorites_screen_home_row_and_picker() {
    let user = htpc_core::User {
        login: "viewer".into(),
        can_chat: true,
    };
    let s = reduce(&home_loaded(), Event::SignedIn(user)).0;
    let mut out = String::from("-- the Favorites screen, nothing starred\n");
    let fav = send(&s, &[Intent::RailPick(RailEntry::Favorites)]);
    out += &render(&view_model(&fav));
    out += &format!("empty_text={:?}\n", view_model(&fav).empty_text);

    out += "-- marrow starred from its card: Home's row\n";
    let starred = send(&s, &[Intent::Down, Intent::Right, Intent::View]);
    out += &popup_lines(&view_model(&starred));
    let starred = send(&starred, &[Intent::Down, Intent::Select]);
    out += &render(&view_model(&starred));

    out += "-- the Favorites screen, live and offline\n";
    let mut s = starred.clone();
    s.favorites.push(ChannelRef::kick("napper"));
    let s = send(&s, &[Intent::RailPick(RailEntry::Favorites)]);
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Favorites,
            rows: vec![
                Row::new(RowKind::FavoritesLive, vec![stream("marrow", 1_900, false)]),
                Row::new(
                    RowKind::FavoritesOffline,
                    vec![Tile::Channel(htpc_core::Channel {
                        channel: ChannelRef::kick("napper"),
                        name: "Napper".into(),
                        tint: Rgb::default(),
                    })],
                ),
            ],
        },
    )
    .0;
    let vm = view_model(&s);
    out += &render(&vm);
    out += &format!("view={:?}\n", vm.view_label);
    let on_offline = send(&s, &[Intent::Down]);
    let vm = view_model(&on_offline);
    out += &format!("on the offline card: view={:?}\n", vm.view_label);
    out += &popup_lines(&view_model(&send(&on_offline, &[Intent::View])));

    out += "-- the picker's Favorites tab, Y on the offline one\n";
    let mut watching = watching_signed_in();
    watching.favorites = s.favorites.clone();
    watching.favorites_feed = s.favorites_feed.clone();
    let picker = send(&watching, &[Intent::View, Intent::Select]);
    let picker = reduce(
        &picker,
        Event::FeedLoaded {
            feed: FeedId::Favorites,
            rows: s.favorites_feed.rows.clone(),
        },
    )
    .0;
    out += &favorite_picker_lines(&view_model(&picker));
    let on_offline = send(&picker, &[Intent::Down]);
    out += &favorite_picker_lines(&view_model(&on_offline));
    out += &favorite_picker_lines(&view_model(&send(&on_offline, &[Intent::ToggleChat])));
    insta::assert_snapshot!(out);
}

#[test]
fn multiview_full_screen_strip_and_pause() {
    let grid = multiview_with(3);
    // Up-left to the stream with sound, A: full screen.
    let full = send(&grid, &[Intent::Up, Intent::Left, Intent::Select]);
    let lines = |vm: &ViewModel| {
        let strip: Vec<String> = vm
            .mv
            .strip
            .iter()
            .map(|s| format!("{}{}{}", s.slot, s.channel, if s.on { "*" } else { "" }))
            .collect();
        format!(
            "screen={:?} channel={:?} video_slot={} position={:?} strip=[{}] select={:?} paused={}\n",
            vm.screen,
            vm.now_playing.channel,
            vm.video_slot,
            vm.mv.position,
            strip.join(" "),
            vm.select_label,
            vm.paused
        )
    };
    let mut out = lines(&view_model(&full));
    let next = send(&full, &[Intent::PageRight]);
    out += &lines(&view_model(&next));
    let paused = send(&next, &[Intent::Select]);
    out += &lines(&view_model(&paused));
    let back = send(&paused, &[Intent::Back]);
    out += &lines(&view_model(&back));
    insta::assert_snapshot!(out);
}

fn compose_lines(vm: &ViewModel) -> String {
    let keys: Vec<&str> = vm.compose_keys.iter().map(|k| k.label.as_str()).collect();
    let focused = usize::try_from(vm.compose_key_focus)
        .ok()
        .and_then(|i| vm.compose_keys.get(i))
        .map_or("-", |k| k.label.as_str());
    format!(
        "open={} needs_sign_in={} overlay={} select={:?} text={:?} count={:?} status={:?} problem={} room={:?} hint={:?}\n  chips={:?} chip_focus={} shift={} key={:?} keys={}\n",
        vm.compose_open,
        vm.compose_needs_sign_in,
        vm.chat_overlay,
        vm.select_label,
        vm.compose_text,
        vm.compose_count,
        vm.compose_status,
        vm.compose_problem,
        vm.room_mode,
        vm.compose_hint,
        vm.compose_chips,
        vm.compose_chip_focus,
        vm.compose_shift,
        focused,
        keys.join(" ")
    )
}

#[test]
fn chat_compose_side_overlay_and_a_drop() {
    let viewer = |can_chat| htpc_core::User {
        login: "viewer".into(),
        can_chat,
    };
    let watching = |user: Option<htpc_core::User>| {
        let mut s = home_loaded();
        s.user = user;
        let (s, _) = reduce(&s, Intent::Select);
        let mut s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
        s.chat_open = true;
        s
    };
    let mut out = String::from("-- signed out\n");
    out += &compose_lines(&view_model(&watching(None)));
    out += "-- signed in, not writing\n";
    let s = watching(Some(viewer(true)));
    out += &compose_lines(&view_model(&s));

    out += "-- side panel, typing with shift on\n";
    let s = send(&s, &[Intent::Retry, Intent::Text('G'), Intent::Text('G')]);
    let s = send(
        &s,
        &[Intent::Down, Intent::Down, Intent::Down, Intent::Select],
    );
    out += &compose_lines(&view_model(&s));

    out += "-- overlay, the emote page\n";
    let mut overlay = send(&s, &[Intent::PageLeft]);
    overlay.settings.chat_layout = ChatLayout::Overlay;
    out += &compose_lines(&view_model(&overlay));

    out += "-- a quick message focused\n";
    let chips = send(
        &s,
        &[
            Intent::Up,
            Intent::Up,
            Intent::Up,
            Intent::Up,
            Intent::Right,
        ],
    );
    out += &compose_lines(&view_model(&chips));

    out += "-- sending, in slow mode\n";
    let s = reduce(
        &s,
        Event::ChatRoomMode {
            channel: ChannelRef::twitch("grandmaster_k"),
            change: htpc_core::RoomModeChange {
                slow_secs: Some(30),
                followers_min: Some(Some(10)),
                subs_only: Some(false),
                emote_only: Some(false),
            },
        },
    )
    .0;
    let s = send(&s, &[Intent::Menu]);
    out += &compose_lines(&view_model(&s));

    out += "-- dropped by slow mode\n";
    let id = s.compose.sending.as_ref().unwrap().id;
    let s = reduce(
        &s,
        Event::ChatDropped {
            id,
            problem: htpc_core::SendProblem::Dropped {
                code: "msg_slowmode".into(),
                message: "This room is in slow mode and you are sending messages too quickly."
                    .into(),
            },
        },
    )
    .0;
    out += &compose_lines(&view_model(&s));

    out += "-- closed with a draft\n";
    let s = send(&s, &[Intent::Back]);
    out += &compose_lines(&view_model(&s));

    out += "-- a sign-in from before chat sending\n";
    let s = send(&watching(Some(viewer(false))), &[]);
    out += &compose_lines(&view_model(&s));
    let s = send(&s, &[Intent::Retry]);
    out += &compose_lines(&view_model(&s));
    insta::assert_snapshot!(out);
}

// ---------------------------------------------------------------------------------------------
// The channel page (vod-plan.md, phase 3)
// ---------------------------------------------------------------------------------------------

/// 2026-11-10 18:28:22 UTC, when the fixture's last broadcast started.
const STREAMED: u64 = 1_794_335_302;
/// Three hours later: when the page was fetched.
const FETCHED: u64 = STREAMED + 3 * 3_600;

fn video(id: &str, title: &str, length_s: u32, created_at: u64) -> Video {
    Video {
        id: id.into(),
        channel: ChannelRef::twitch("sleepy_fox"),
        channel_name: "Sleepy Fox".into(),
        kind: VideoKind::Archive,
        title: title.into(),
        category: "Hollow Knight".into(),
        length_s,
        created_at,
        views: 0,
        tint: Rgb::from_hex(0x2d4a6b),
        thumbnail: Some(format!("https://cdn/{id}.jpg")),
        muted: Vec::new(),
        sub_only: false,
        recording: false,
    }
}

fn clip(slug: &str, title: &str, views: u64, created_at: u64) -> Clip {
    Clip {
        slug: slug.into(),
        channel: ChannelRef::twitch("sleepy_fox"),
        title: title.into(),
        category: "Hollow Knight".into(),
        length_s: 28,
        views,
        clipped_by: "mossy_k".into(),
        created_at,
        tint: Rgb::from_hex(0x6b2d3c),
        thumbnail: None,
    }
}

/// The page of the offline channel `sleepy_fox`, opened from its card on Home and loaded: three
/// broadcasts (one with a muted part, one subscriber-only, one from last year), a highlight and
/// four clips in two rows.
fn channel_page() -> AppState {
    let offline = Tile::Channel(htpc_core::Channel {
        channel: ChannelRef::twitch("sleepy_fox"),
        name: "sleepy_fox".into(),
        tint: Rgb::default(),
    });
    let (s, _) = init();
    let (s, _) = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(RowKind::TopLive, vec![offline])],
        },
    );
    let s = send(&s, &[Intent::Select]);
    assert_eq!(s.screen, htpc_core::Screen::Channel);
    let rows = vec![
        Row::new(
            RowKind::PastBroadcasts,
            vec![
                Tile::Video(Video {
                    muted: vec![Span {
                        start_s: 5_400,
                        length_s: 360,
                    }],
                    ..video("9001", "No-hit run attempts, night 12", 15_128, STREAMED)
                }),
                Tile::Video(Video {
                    sub_only: true,
                    ..video(
                        "9002",
                        "Subathon recap and Q&A",
                        7_435,
                        STREAMED - 4 * 86_400,
                    )
                }),
                Tile::Video(video(
                    "9003",
                    "Night 11: the boss that will not die",
                    20_421,
                    1_740_000_000,
                )),
            ],
        ),
        Row::new(
            RowKind::Highlights,
            vec![Tile::Video(Video {
                kind: VideoKind::Highlight,
                ..video("9004", "The 0.4 s dodge", 764, STREAMED)
            })],
        ),
        Row::new(
            RowKind::ClipsWeek,
            vec![
                Tile::Clip(clip("ThePerfectDodge", "the 0.4 s dodge", 41_000, STREAMED)),
                Tile::Clip(clip(
                    "CatOnKeys",
                    "cat on the keyboard",
                    860,
                    STREAMED - 86_400,
                )),
            ],
        ),
        Row::new(
            RowKind::ClipsAllTime,
            vec![Tile::Clip(clip(
                "WorldRecord",
                "world record, chat goes wild",
                412_000,
                1_740_000_000,
            ))],
        ),
    ];
    let info = ChannelInfo {
        channel: ChannelRef::twitch("sleepy_fox"),
        user_id: "77".into(),
        name: "Sleepy Fox".into(),
        avatar: Some("https://cdn/avatar.jpg".into()),
        live: None,
        last_live_at: Some(STREAMED),
        loaded_at: FETCHED,
    };
    reduce(
        &s,
        Event::ChannelPageLoaded {
            info: Box::new(info),
            rows,
        },
    )
    .0
}

fn channel_lines(vm: &ViewModel) -> String {
    format!(
        "name={:?} initial={:?} avatar={:?} live={} status={:?} tab={} favorite={}\n\
         select={:?} start_over={} retry={} back={} channel_hint={} view={:?} empty={} empty_text={:?} error={:?}\n",
        vm.channel_name,
        vm.channel_initial,
        vm.channel_avatar,
        vm.channel_live,
        vm.channel_status,
        vm.channel_tab,
        vm.channel_favorite,
        vm.select_label,
        vm.can_start_over,
        vm.can_retry,
        vm.can_back,
        vm.can_channel,
        vm.view_label,
        vm.feed_empty,
        vm.empty_text,
        vm.feed_error
    )
}

#[test]
fn channel_page_videos_tab() {
    let mut out = String::from("-- opened from the offline card, loading\n");
    let offline = Tile::Channel(htpc_core::Channel {
        channel: ChannelRef::twitch("sleepy_fox"),
        name: "Sleepy Fox".into(),
        tint: Rgb::default(),
    });
    let (s, _) = init();
    let s = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(RowKind::TopLive, vec![offline])],
        },
    )
    .0;
    // The card's own hints first: Y opens the page, A says so.
    let vm = view_model(&s);
    out += &format!(
        "on the card: select={:?} channel_hint={}\n",
        vm.select_label, vm.can_channel
    );
    let s = send(&s, &[Intent::ToggleChat]);
    let vm = view_model(&s);
    out += &channel_lines(&vm);
    out += &render(&vm);

    out += "-- loaded, the first broadcast focused\n";
    let s = channel_page();
    let vm = view_model(&s);
    out += &channel_lines(&vm);
    out += &render(&vm);

    out += "-- the subscriber-only broadcast\n";
    let on_sub = send(&s, &[Intent::Right]);
    out += &channel_lines(&view_model(&on_sub));
    out += "-- the broadcast from last year, then down to the highlight\n";
    let on_old = send(&on_sub, &[Intent::Right]);
    out += &channel_lines(&view_model(&on_old));
    let on_highlight = send(&on_old, &[Intent::Down]);
    let vm = view_model(&on_highlight);
    out += &channel_lines(&vm);
    out += &format!(
        "focus=({}, {}) row_cols={:?}\n",
        vm.sel_row, vm.sel_col, vm.row_cols
    );

    out += "-- the first broadcast was left at 25:00 (Continue watching, phase 5)\n";
    let mut saved = s.clone();
    saved.continue_watching = vec![Progress {
        video: video("9001", "No-hit run attempts, night 12", 15_128, STREAMED),
        pos_s: 1_500,
        saved_at: FETCHED,
    }];
    let vm = view_model(&saved);
    out += &channel_lines(&vm);
    out += &render(&vm);

    out += "-- View: added to favorites (a star in the header), then View again: removed\n";
    let starred = send(&s, &[Intent::View]);
    out += &channel_lines(&view_model(&starred));
    out += &channel_lines(&view_model(&send(&starred, &[Intent::View])));

    out += "-- live: the Live now row comes first\n";
    let live = Stream {
        channel: ChannelRef::twitch("sleepy_fox"),
        user_id: "77".into(),
        title: "No-hit run, night 13".into(),
        category: "Hollow Knight".into(),
        viewers: 1_200,
        tint: Rgb::default(),
        thumbnail: None,
    };
    let mut rows = s.channel_page.as_ref().unwrap().videos.rows.clone();
    rows.insert(
        0,
        Row::new(RowKind::LiveNow, vec![Tile::Stream(live.clone())]),
    );
    let info = ChannelInfo {
        channel: ChannelRef::twitch("sleepy_fox"),
        user_id: "77".into(),
        name: "Sleepy Fox".into(),
        avatar: None,
        live: Some(live),
        last_live_at: Some(FETCHED),
        loaded_at: FETCHED,
    };
    let now_live = reduce(
        &send(&s, &[Intent::Back, Intent::Select]),
        Event::ChannelPageLoaded {
            info: Box::new(info),
            rows,
        },
    )
    .0;
    let vm = view_model(&now_live);
    out += &channel_lines(&vm);
    out += &render(&vm);
    insta::assert_snapshot!(out);
}

#[test]
fn home_continue_watching_row() {
    let mut s = home_loaded();
    s.continue_watching = vec![
        Progress {
            video: video("9001", "No-hit run attempts, night 12", 15_128, STREAMED),
            pos_s: 1_500,
            saved_at: FETCHED,
        },
        Progress {
            video: video("9004", "The 0.4 s dodge", 764, STREAMED - 86_400),
            pos_s: 400,
            saved_at: FETCHED - 3_600,
        },
    ];
    // The row appears with the list, without a Home reload.
    let mut out = String::from("-- the row after Followed\n");
    let hints = |vm: &ViewModel| {
        format!(
            "select={:?} remove={} channel_hint={} view={:?}\n",
            vm.select_label, vm.can_remove, vm.can_channel, vm.view_label
        )
    };
    let s = reduce(&s, Event::FeedRefreshDue).0;
    let vm = view_model(&s);
    out += &render(&vm);
    out += "-- on its first card: A resumes, X removes\n";
    let on_card = send(&s, &[Intent::Down, Intent::Down]);
    let vm = view_model(&on_card);
    out += &hints(&vm);
    out += &render(&vm);
    out += "-- on a stream card: no X\n";
    out += &hints(&view_model(&s));
    out += "-- after X on the first card\n";
    let removed = send(&on_card, &[Intent::Retry]);
    let vm = view_model(&removed);
    out += &hints(&vm);
    out += &render(&vm);
    insta::assert_snapshot!(out);
}

#[test]
fn channel_page_clips_tab() {
    let s = send(&channel_page(), &[Intent::PageRight]);
    let mut out = String::from("-- the Clips tab\n");
    let vm = view_model(&s);
    out += &channel_lines(&vm);
    out += &render(&vm);

    out += "-- back on Videos, and a failed load with no rows\n";
    let back = send(&s, &[Intent::PageLeft]);
    out += &channel_lines(&view_model(&back));
    let (home, _) = reduce(&back, Intent::Back);
    let channel = ChannelRef::twitch("sleepy_fox");
    let (failed, _) = reduce(
        &send(&home, &[Intent::Select]),
        Event::ChannelPageFailed {
            channel,
            message: "network error: timed out".into(),
        },
    );
    let vm = view_model(&failed);
    out += &channel_lines(&vm);
    out += &render(&vm);

    out += "-- a channel with nothing on the Clips tab\n";
    let mut empty = channel_page();
    let page = empty.channel_page.as_mut().unwrap();
    page.clips.rows.clear();
    let empty = send(&empty, &[Intent::PageRight]);
    let vm = view_model(&empty);
    out += &channel_lines(&vm);
    out += &render(&vm);
    insta::assert_snapshot!(out);
}

#[test]
fn channel_page_subscribe_card() {
    let mut s = channel_page();
    s.can_open_browser = true;
    let on_sub = send(&s, &[Intent::Right]);
    let card = send(&on_sub, &[Intent::Select]);
    let vm = view_model(&card);
    let mut out = format!(
        "card={} url={:?} qr={}x{} can_open={} select={:?}\n",
        vm.subscribe_open,
        vm.subscribe_url,
        vm.subscribe_qr_width,
        vm.subscribe_qr_width,
        vm.subscribe_can_open,
        vm.select_label
    );
    s.can_open_browser = false;
    let card = send(&send(&s, &[Intent::Right]), &[Intent::Select]);
    out += &format!("game mode: select={:?}\n", view_model(&card).select_label);
    let closed = send(&card, &[Intent::Back]);
    let vm = view_model(&closed);
    out += &format!(
        "closed: card={} screen={:?} select={:?}\n",
        vm.subscribe_open, vm.screen, vm.select_label
    );
    insta::assert_snapshot!(out);
}

// ---------------------------------------------------------------------------------------------
// The video player (vod-plan.md, phase 4)
// ---------------------------------------------------------------------------------------------

fn video_lines(vm: &ViewModel) -> String {
    let v = &vm.video;
    let muted: Vec<(i32, i32)> = v.muted.iter().map(|m| (m.start, m.width)).collect();
    format!(
        "screen={:?} playback={:?} paused={} select={:?} retry={} hints_menu={}\n\
         \x20 top: [{}] {:?} | {:?} | {:?}\n\
         \x20 bar: {} / {:?} head={} chapter={:?} ticks={:?} muted={:?} in_muted={}\n\
         \x20 seek: chip={:?} side={} seeking={} scrubbing={} speed={:?} chapters={} end_card={} end_focus={}\n\
         \x20 stream card: {:?} {:?}\n",
        vm.screen,
        vm.playback,
        vm.paused,
        vm.select_label,
        vm.can_retry,
        vm.player_menu_open,
        v.kind,
        v.channel,
        v.title,
        v.meta,
        v.pos,
        v.length,
        v.head,
        v.chapter,
        v.ticks,
        muted,
        v.in_muted,
        v.chip,
        v.chip_side,
        v.seeking,
        v.scrubbing,
        v.speed,
        v.chapters,
        v.end_card,
        v.end_focus,
        vm.now_playing.channel,
        vm.now_playing.title,
    )
}

fn video_menu_lines(vm: &ViewModel) -> String {
    let mut out = format!(
        "menu_open={} focus={} select={:?}\n",
        vm.player_menu_open, vm.player_menu_focus, vm.select_label
    );
    for (i, row) in vm.player_menu_rows.iter().enumerate() {
        let mark = if i as i32 == vm.player_menu_focus {
            ">"
        } else {
            " "
        };
        let kind = if row.action { " (action)" } else { "" };
        out += &format!("  {mark} {} = {:?}{kind}\n", row.title, row.value);
    }
    if let Some(row) = vm.player_menu_rows.get(vm.player_menu_focus as usize) {
        out += &format!("  help: {}\n", row.help);
    }
    out
}

/// The id of the last seek timer the commands asked for.
fn seek_timer(cmds: &[htpc_core::Command]) -> u32 {
    cmds.iter()
        .rev()
        .find_map(|c| match c {
            htpc_core::Command::HoldCheck { id }
            | htpc_core::Command::ScrubTick { id }
            | htpc_core::Command::SettleSeek { id, .. } => Some(*id),
            _ => None,
        })
        .expect("a seek timer")
}

fn video_health(pos_ms: u64) -> Event {
    Event::PlayerHealth {
        slot: 0,
        at_ms: 0,
        health: htpc_core::Health {
            stalled: false,
            buffer_ms: 60_000,
            pos_ms: Some(pos_ms),
            length_ms: Some(15_128_000),
        },
    }
}

/// The first broadcast of the channel page, playing at 1:12:40 with its five chapters.
fn video_playing() -> AppState {
    let s = send(&channel_page(), &[Intent::Select]);
    let s = reduce(&s, Event::PlaybackStarted { slot: 0 }).0;
    let chapter = |start_ms: u64, name: &str| htpc_core::Chapter {
        start_ms,
        length_ms: 0,
        name: name.into(),
    };
    let details = htpc_core::VideoDetails {
        id: "9001".into(),
        length_s: 15_128,
        chapters: vec![
            chapter(0, "Just Chatting"),
            chapter(1_260_000, "Hollow Depths"),
            chapter(7_800_000, "Night Circuit"),
            chapter(11_400_000, "Hollow Depths"),
            chapter(13_900_000, "Just Chatting"),
        ],
        muted: vec![
            Span {
                start_s: 5_400,
                length_s: 360,
            },
            Span {
                start_s: 11_020,
                length_s: 360,
            },
        ],
        previews: None,
    };
    let s = reduce(&s, Event::VideoDetails(details)).0;
    reduce(&s, video_health(4_360_000)).0
}

#[test]
fn video_player_playing_paused_seeking_and_menu() {
    let mut out = String::from("-- A on the first broadcast: loading\n");
    let loading = send(&channel_page(), &[Intent::Select]);
    out += &video_lines(&view_model(&loading));

    out += "-- playing at 1:12:40, with chapters and muted parts\n";
    let s = video_playing();
    out += &video_lines(&view_model(&s));

    out += "-- paused\n";
    let paused = send(&s, &[Intent::Select]);
    out += &video_lines(&view_model(&paused));

    out += "-- two taps forward, adding up\n";
    let (taps, _) = reduce(&s, Intent::Right);
    let (taps, _) = reduce(&taps, Intent::Released(htpc_core::Dir::Right));
    let (taps, _) = reduce(&taps, Intent::Right);
    out += &video_lines(&view_model(&taps));
    out += "-- one tap back\n";
    let (back, _) = reduce(&s, Intent::Left);
    out += &video_lines(&view_model(&back));

    out += "-- held: scrubbing forward, a few steps in\n";
    let (held, cmds) = reduce(&s, Intent::Right);
    let (mut held, mut cmds) = reduce(&held, Event::HoldDue(seek_timer(&cmds)));
    for _ in 0..12 {
        let (next, more) = reduce(&held, Event::ScrubTickDue(seek_timer(&cmds)));
        held = next;
        cmds = more;
    }
    out += &video_lines(&view_model(&held));
    out += "-- let go: the jump\n";
    let (released, cmds) = reduce(&held, Intent::Released(htpc_core::Dir::Right));
    let (jumped, _) = reduce(&released, Event::SeekSettleDue(seek_timer(&cmds)));
    out += &video_lines(&view_model(&jumped));

    out += "-- in a muted part\n";
    let muted = reduce(&s, video_health(5_460_000)).0;
    out += &video_lines(&view_model(&muted));

    out += "-- the menu: a video's rows\n";
    let menu = send(&s, &[Intent::Menu]);
    out += &video_menu_lines(&view_model(&menu));
    out += "-- speed 1.5x\n";
    let fast = send(&menu, &[Intent::Down, Intent::Right, Intent::Right]);
    out += &video_menu_lines(&view_model(&fast));
    out += "-- the chapter list, pointing at the next chapter\n";
    let chapters = send(&fast, &[Intent::Down, Intent::Right]);
    out += &video_menu_lines(&view_model(&chapters));
    out += "-- start over, and the HUD with the speed\n";
    let start_over = send(&chapters, &[Intent::Down]);
    out += &video_menu_lines(&view_model(&start_over));
    let closed = send(&fast, &[Intent::Back]);
    out += &video_lines(&view_model(&closed));

    out += "-- the end card\n";
    let ended = reduce(&s, Event::PlaybackEnded { slot: 0 }).0;
    out += &video_lines(&view_model(&ended));
    let on_back = send(&ended, &[Intent::Down, Intent::Down]);
    out += &video_lines(&view_model(&on_back));

    out += "-- a video that is gone\n";
    let gone = reduce(
        &loading,
        Event::PlaybackError {
            slot: 0,
            message: "This video is no longer available.".into(),
        },
    )
    .0;
    out += &video_lines(&view_model(&gone));
    out += &format!("  error={:?}\n", view_model(&gone).playback_error);
    insta::assert_snapshot!(out);
}

#[test]
fn video_resume_toast_and_a_clip() {
    let mut s = channel_page();
    s.continue_watching = vec![Progress {
        video: video("9001", "No-hit run attempts, night 12", 15_128, STREAMED),
        pos_s: 4_360,
        saved_at: FETCHED,
    }];
    let resumed = send(&s, &[Intent::Select]);
    let vm = view_model(&resumed);
    let mut out = toast_lines(&vm);
    out += &video_lines(&vm);

    out += "-- a clip\n";
    let clip = send(&channel_page(), &[Intent::PageRight, Intent::Select]);
    let clip = reduce(&clip, Event::PlaybackStarted { slot: 0 }).0;
    let vm = view_model(&clip);
    out += &video_lines(&vm);
    let menu = send(&clip, &[Intent::Menu]);
    let vm = view_model(&menu);
    out += &video_menu_lines(&vm);
    assert!(
        !menu.player_menu_rows().contains(&PlayerMenuRow::Chapters),
        "a clip has no chapters"
    );
    insta::assert_snapshot!(out);
}

/// What the chat replay panel shows: header, status, lines, the compose line and the hint.
fn replay_lines(vm: &ViewModel) -> String {
    format!(
        "title={:?} compose_hint={:?} compose_open={} hint_y={}\n{}{}",
        vm.chat_title,
        vm.compose_hint,
        vm.compose_open,
        vm.video.replay,
        chat_lines(vm),
        toast_lines(vm),
    )
}

fn replay_line(offset_s: u32, user: &str, spans: Vec<ChatSpan>) -> htpc_core::ReplayLine {
    htpc_core::ReplayLine {
        offset_s,
        message: ChatMessage {
            id: format!("c{offset_s}"),
            spans,
            ..ChatMessage::plain(user, "")
        },
    }
}

#[test]
fn chat_replay_panel_loading_lines_and_no_replay() {
    let mut out = String::from("-- playing, panel closed\n");
    let s = video_playing();
    out += &replay_lines(&view_model(&s));

    out += "-- Y: the panel opens, fetching\n";
    let (open, cmds) = reduce(&s, Intent::ToggleChat);
    out += &format!("requests: {cmds:?}\n");
    out += &replay_lines(&view_model(&open));

    out += "-- the window: lines up to the playhead show, with an emote\n";
    let text = |t: &str| vec![ChatSpan::Text(t.into())];
    let lines = vec![
        replay_line(4_330, "mossy_k", text("that dodge was clean")),
        replay_line(
            4_345,
            "Ember_Rae",
            vec![
                ChatSpan::Text("no way".into()),
                ChatSpan::Emote {
                    name: "Kappa".into(),
                    url: "https://cdn/kappa".into(),
                },
            ],
        ),
        replay_line(4_360, "nightowl_dev", text("here we go")),
        replay_line(4_375, "mossy_k", text("not due yet")),
    ];
    let loaded = reduce(
        &open,
        Event::ChatReplay {
            id: "9001".into(),
            offset_s: 4_360,
            lines,
        },
    )
    .0;
    out += &replay_lines(&view_model(&loaded));

    out += "-- the playhead passes the next line\n";
    let later = reduce(&loaded, video_health(4_376_000)).0;
    out += &replay_lines(&view_model(&later));

    out += "-- a jump clears the panel and fetches again\n";
    let (jumped, cmds) = reduce(&later, Intent::PageRight);
    out += &format!("requests: {cmds:?}\n");
    out += &replay_lines(&view_model(&jumped));

    out += "-- Twitch gives no replay: the panel closes, said once\n";
    let (refused, _) = reduce(&s, Intent::ToggleChat);
    let refused = reduce(
        &refused,
        Event::ChatReplayFailed {
            id: "9001".into(),
            offset_s: 4_360,
        },
    )
    .0;
    out += &replay_lines(&view_model(&refused));
    let (again, cmds) = reduce(&refused, Intent::ToggleChat);
    out += &format!(
        "Y again: {} commands, {} toasts\n",
        cmds.len(),
        again.toasts.len()
    );

    out += "-- a clip has no replay hint\n";
    let clip = send(&channel_page(), &[Intent::PageRight, Intent::Select]);
    let clip = reduce(&clip, Event::PlaybackStarted { slot: 0 }).0;
    out += &replay_lines(&view_model(&clip));
    insta::assert_snapshot!(out);
}

#[test]
fn dates_and_ages_read_as_people_say_them() {
    // 2026-11-10 18:28:22 UTC.
    let now = STREAMED + 3 * 3_600;
    assert_eq!(format_date(STREAMED, now), "Nov 10");
    assert_eq!(format_date(STREAMED - 86_400 * 12, now), "Oct 29");
    assert_eq!(format_date(1_740_000_000, now), "Feb 19, 2025");
    assert_eq!(format_date(0, now), "");
    assert_eq!(format_date(STREAMED, 0), "Nov 10", "no clock: no year");
    for (secs, text) in [
        (0, "just now"),
        (59, "just now"),
        (60, "1 min ago"),
        (3_599, "59 min ago"),
        (3_600, "1 hour ago"),
        (3 * 3_600, "3 hours ago"),
        (86_400, "1 day ago"),
        (29 * 86_400, "29 days ago"),
        (30 * 86_400, "1 month ago"),
        (400 * 86_400, "1 year ago"),
        (800 * 86_400, "2 years ago"),
    ] {
        assert_eq!(format_ago(now - secs, now), text, "{secs} s");
    }
    assert_eq!(
        format_ago(now + 500, now),
        "just now",
        "a clock that runs behind"
    );
}
