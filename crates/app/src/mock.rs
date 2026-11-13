//! `--mock` fixture data: the scaffold's rows and chat, served through `htpc_twitch_api::FakeApi`
//! so mock mode exercises the same `ApiProvider` mapping and `Event`s as the real Helix client.

use std::time::Duration;

use htpc_chat::{ChatEvent, FakeChat};
use htpc_core::{ChatKind, ChatMessage, ChatSpan, ChatStatus, Rgb};
use htpc_kick_api::{fake_live, FakeKick};
use htpc_twitch_api::{
    ClipInfo, FakeApi, FollowedChannel, Game, HypeTrain, MutedSegment, ReplayComment,
    ReplayFragment, StreamInfo, UserInfo, VideoChapter, VideoInfo, VideoType,
};

/// The device code the store tests' fake Twitch hands out (`--mock` shows `FakeAuth`'s own,
/// which is the same one as the scaffold's).
#[cfg(test)]
pub const MOCK_USER_CODE: &str = "KQRT-7ZMP";
/// The user that the mock sign-in approves.
pub const MOCK_USER: &str = "nightowl_dev";
/// Helix user id of [`MOCK_USER`]. The session hands it to the provider after the mock sign-in,
/// which is what shows the Followed row.
pub const MOCK_USER_ID: &str = "1000";

fn followed(login: &str) -> FollowedChannel {
    FollowedChannel {
        broadcaster_id: format!("id-{login}"),
        broadcaster_login: login.into(),
        broadcaster_name: login.into(),
    }
}

fn stream(channel: &str, title: &str, category: &str, viewers: u64) -> StreamInfo {
    StreamInfo {
        user_id: format!("id-{channel}"),
        user_login: channel.into(),
        user_name: channel.into(),
        game_id: format!("game-{category}"),
        game_name: category.into(),
        title: title.into(),
        viewer_count: viewers,
        thumbnail_url: String::new(),
    }
}

fn game(name: &str) -> Game {
    Game {
        id: format!("game-{name}"),
        name: name.into(),
        box_art_url: String::new(),
    }
}

/// A `FakeApi` with the scaffold's fixture rows, signed in as [`MOCK_USER`].
pub fn fake_api() -> FakeApi {
    FakeApi {
        followed: vec![
            stream(
                "nightowl_dev",
                "Rewriting my compositor in Rust",
                "Software & Game Dev",
                4_200,
            ),
            stream(
                "pixelpilgrim",
                "Blind run, no hit challenge",
                "Hollow Knight",
                12_800,
            ),
            stream("marrow", "Sunday speedrun practice", "Celeste", 1_900),
            stream("lofi_lena", "Late night beats", "Music", 860),
            stream("quietcoast", "Cozy farm day 40", "Stardew Valley", 3_100),
        ],
        // The five live ones above, and three more who are offline right now.
        follows: [
            "nightowl_dev",
            "pixelpilgrim",
            "marrow",
            "lofi_lena",
            "quietcoast",
            "sleepy_fox",
            "driftwood",
            "kettle_kai",
        ]
        .into_iter()
        .map(followed)
        .collect(),
        top: vec![
            stream(
                "grandmaster_k",
                "Ranked climb to Radiant",
                "VALORANT",
                48_100,
            ),
            stream("tidewatch", "World first attempts", "Elden Ring", 31_500),
            stream("brickby", "Building a 1:1 castle", "Minecraft", 22_400),
            stream("sable", "Just chatting + drawing", "Just Chatting", 19_700),
            stream("octave", "Road to Champion", "Rocket League", 14_000),
        ],
        games: [
            "Just Chatting",
            "Hollow Knight",
            "VALORANT",
            "Software & Game Dev",
            "Music",
        ]
        .into_iter()
        .map(game)
        .collect(),
        users: Vec::new(),
        me: Some(UserInfo {
            id: MOCK_USER_ID.into(),
            login: MOCK_USER.into(),
            display_name: MOCK_USER.into(),
            profile_image_url: String::new(),
        }),
        // The top stream has a hype train going, so the bar can be seen.
        hype_trains: vec![(
            "grandmaster_k".into(),
            HypeTrain {
                id: "mock-train".into(),
                level: 3,
                progress: 1_250,
                goal: 2_000,
                seconds_left: 240,
                all_time_high: 7,
            },
        )],
        videos: mock_videos(),
        clips_week: vec![
            mock_clip(
                "sleepy_fox",
                "ThePerfectDodge",
                "the 0.4 s dodge",
                28,
                41_000,
                "2026-11-10",
            ),
            mock_clip(
                "sleepy_fox",
                "CatOnTheKeyboard",
                "the cat walks across the keyboard",
                33,
                31_000,
                "2026-11-07",
            ),
        ],
        clips_all_time: vec![
            mock_clip(
                "sleepy_fox",
                "WorldRecordChatGoesWild",
                "world record, chat goes wild",
                52,
                412_000,
                "2025-03-02",
            ),
            mock_clip(
                "sleepy_fox",
                "ThePerfectDodge",
                "the 0.4 s dodge",
                28,
                41_000,
                "2026-11-10",
            ),
            mock_clip(
                "driftwood",
                "BoatGoesBrrr",
                "boat goes brrr",
                19,
                2_400,
                "2026-10-01",
            ),
        ],
        // The first broadcast's game changes, for the chapter list and LB / RB.
        chapters: vec![(
            "9000000001".into(),
            vec![
                mock_chapter(0, 1_260, "Just Chatting"),
                mock_chapter(1_260, 6_540, "Hollow Knight"),
                mock_chapter(7_800, 7_328, "Celeste"),
            ],
        )],
        // Every mock video plays the same ten minutes, so each has a quarter of an hour of chat.
        replays: mock_videos()
            .iter()
            .filter(|v| !v.sub_only)
            .map(|v| (v.id.clone(), mock_replay()))
            .collect(),
        // Messages are "posted" and kept, but nothing echoes them back into the mock chat.
        ..FakeApi::new()
    }
}

/// A made-up chat replay: a line every few seconds for 15 minutes, some with emotes.
fn mock_replay() -> Vec<ReplayComment> {
    const USERS: [(&str, &str); 6] = [
        ("mossy_k", "#3FA34D"),
        ("nightowl_dev", "#8A2BE2"),
        ("ember_rae", "#FF7F50"),
        ("tin_whistle", ""),
        ("cat_on_keys", "#1E90FF"),
        ("fox_fan_99", "#DAA520"),
    ];
    // Text, and an emote id to put after it (`25` is Kappa, `41` Kreygasm, `86` BibleThump).
    const SAYS: [(&str, Option<(&str, &str)>); 8] = [
        ("that dodge was clean", None),
        ("", Some(("Kappa", "25"))),
        ("here we go", Some(("Kreygasm", "41"))),
        ("no way he survives this", None),
        ("how many attempts is this", None),
        ("", Some(("BibleThump", "86"))),
        ("the music slaps tonight", None),
        ("chat is so back", Some(("Kappa", "25"))),
    ];
    let mut at = 2;
    (0..)
        .map_while(|i: usize| {
            let (login, color) = USERS[(i * 5 + i / 3) % USERS.len()];
            let (text, emote) = SAYS[(i * 3 + i / 7) % SAYS.len()];
            let mut fragments = Vec::new();
            if !text.is_empty() {
                fragments.push(ReplayFragment {
                    text: format!("{text} "),
                    emote_id: None,
                });
            }
            if let Some((name, id)) = emote {
                fragments.push(ReplayFragment {
                    text: name.into(),
                    emote_id: Some(id.into()),
                });
            }
            let line = ReplayComment {
                id: format!("mock-{i}"),
                offset_seconds: at,
                login: login.into(),
                name: login.into(),
                color: color.into(),
                fragments,
            };
            at += 2 + (i as u32 * 7) % 5;
            (line.offset_seconds <= 900).then_some(line)
        })
        .collect()
}

/// The mock channels' videos (vod-plan.md). Two offline followed channels have past broadcasts;
/// sleepy_fox also has highlights, a muted part, a subscriber-only broadcast and clips. A live
/// channel, pixelpilgrim, has its broadcast still recording.
fn mock_videos() -> Vec<VideoInfo> {
    let fox = |id: &str, kind, title: &str, length: u32, day: &str, game: &str| VideoInfo {
        id: id.into(),
        kind,
        title: title.into(),
        length_seconds: length,
        created_at: format!("{day}T18:00:00Z"),
        view_count: u64::from(length) * 3,
        game_name: game.into(),
        thumbnail_url: String::new(),
        recording: false,
        sub_only: false,
        muted: Vec::new(),
        owner_login: "sleepy_fox".into(),
        owner_name: "sleepy_fox".into(),
    };
    vec![
        VideoInfo {
            muted: vec![MutedSegment {
                offset: 5_400,
                duration: 360,
            }],
            ..fox(
                "9000000001",
                VideoType::Archive,
                "No-hit run attempts, night 12",
                15_128,
                "2026-11-10",
                "Just Chatting",
            )
        },
        fox(
            "9000000002",
            VideoType::Archive,
            "Night 11: the boss that will not die",
            20_421,
            "2026-11-09",
            "Hollow Knight",
        ),
        VideoInfo {
            sub_only: true,
            ..fox(
                "9000000003",
                VideoType::Archive,
                "Subathon recap and Q&A",
                7_435,
                "2026-11-06",
                "Just Chatting",
            )
        },
        fox(
            "9000000004",
            VideoType::Highlight,
            "The 0.4 s dodge that saved night 12",
            764,
            "2026-11-10",
            "Hollow Knight",
        ),
        fox(
            "9000000005",
            VideoType::Highlight,
            "Best of October",
            2_910,
            "2026-11-01",
            "Hollow Knight",
        ),
        VideoInfo {
            owner_login: "driftwood".into(),
            owner_name: "driftwood".into(),
            ..fox(
                "9000000006",
                VideoType::Archive,
                "Sailing the long way round",
                9_870,
                "2026-11-08",
                "Sea of Thieves",
            )
        },
        VideoInfo {
            recording: true,
            owner_login: "pixelpilgrim".into(),
            owner_name: "pixelpilgrim".into(),
            ..fox(
                "9000000007",
                VideoType::Archive,
                "Blind run, no hit challenge",
                5_400,
                "2026-11-11",
                "Hollow Knight",
            )
        },
    ]
}

fn mock_clip(
    login: &str,
    slug: &str,
    title: &str,
    length: u32,
    views: u64,
    day: &str,
) -> (String, ClipInfo) {
    (
        login.into(),
        ClipInfo {
            slug: slug.into(),
            title: title.into(),
            duration_seconds: length,
            view_count: views,
            created_at: format!("{day}T20:00:00Z"),
            thumbnail_url: String::new(),
            curator_name: "mossy_k".into(),
            game_name: "Hollow Knight".into(),
        },
    )
}

fn mock_chapter(start_s: u64, length_s: u64, name: &str) -> VideoChapter {
    VideoChapter {
        position_ms: start_s * 1000,
        duration_ms: length_s * 1000,
        name: name.into(),
    }
}

/// The lines every mock channel starts with.
pub fn mock_chat() -> Vec<ChatMessage> {
    let line = |user: &str, color: Option<Rgb>, text: &str| ChatMessage {
        color,
        ..ChatMessage::plain(user, text)
    };
    vec![
        // `None` uses the view-model's default name colour, the scaffold's lime.
        line("marrow", None, "that dodge was clean"),
        line(
            "lofi_lena",
            Some(Rgb::new(255, 105, 180)),
            "good evening everyone",
        ),
        line(
            "tidewatch",
            Some(Rgb::new(30, 144, 255)),
            "chat is this the new build?",
        ),
        line("brickby", Some(Rgb::new(255, 140, 0)), "no way he lands it"),
        line("sable", None, "LETS GOOOO"),
        line(
            "octave",
            Some(Rgb::new(0, 255, 127)),
            "audio is perfect today",
        ),
    ]
}

/// Lines that keep arriving in mock mode, so the live chat can be seen moving. They show the
/// other kinds of line: a /me, an emote (its name, as mock mode downloads nothing), a notice.
fn mock_chat_later() -> Vec<ChatMessage> {
    let plain = |user: &str, color: Option<Rgb>, text: &str| ChatMessage {
        color,
        ..ChatMessage::plain(user, text)
    };
    vec![
        ChatMessage {
            kind: ChatKind::Action,
            color: Some(Rgb::new(255, 105, 180)),
            ..ChatMessage::plain("lofi_lena", "waves at chat")
        },
        ChatMessage {
            spans: vec![
                ChatSpan::Text("that was incredible".into()),
                ChatSpan::Emote {
                    name: "Kappa".into(),
                    url: "https://static-cdn.jtvnw.net/emoticons/v2/25/static/dark/2.0".into(),
                },
            ],
            ..plain("brickby", Some(Rgb::new(255, 140, 0)), "")
        },
        plain(
            "marrow",
            None,
            "a very long message that has to wrap onto a second line so the layout can be checked",
        ),
        ChatMessage::notice("This room is now in slow mode."),
        plain("tidewatch", Some(Rgb::new(30, 144, 255)), "gg"),
    ]
}

/// Chat for `--mock` and tests: the fixture lines at once, and, if `lively`, a new line every
/// couple of seconds.
pub fn fake_chat(lively: bool) -> FakeChat {
    let script = vec![
        ChatEvent::Status(ChatStatus::Live),
        ChatEvent::Messages(mock_chat()),
    ];
    let chat = FakeChat::new(script);
    if lively {
        chat.trickling(mock_chat_later(), Duration::from_millis(2500))
    } else {
        chat
    }
}

/// The mock channels that are not live: following them shows them offline, and playing one ends on
/// the offline screen.
const MOCK_OFFLINE: [&str; 4] = ["sleepy_fox", "driftwood", "kettle_kai", "sleepy-fox"];

/// How long a mock resolve takes, so "Starting…" and the quality swap can be seen.
const MOCK_RESOLVE: Duration = Duration::from_millis(600);

/// Picture sources from FFmpeg's `lavfi` that move, so a playing tile is plain to see.
/// Each is a source and its options after the size and rate.
const MOCK_PATTERNS: [(&str, &str); 6] = [
    ("testsrc2", ""),
    ("gradients", ":speed=0.02"),
    (
        "life",
        ":mold=10:ratio=0.2:life_color=0xa970ff:death_color=0x1a1030",
    ),
    ("testsrc", ""),
    ("gradients", ":c0=0x53fc18:c1=0x0e0e10:speed=0.03"),
    ("cellauto", ":rule=110"),
];

/// `--mock`'s stream lookup: every live channel plays a generated test picture with a quiet tone,
/// through FFmpeg's `lavfi` device in mpv, so the player and multiview work without a network. The
/// picture is as big as the rendition asked for (a 480p tile gets 854x480), so quality changes show
/// in the stats box, and each channel has its own picture and pitch, so moving the sound can be
/// heard.
///
/// A video or clip plays a generated file of a known length ([`MOCK_VIDEO`], [`MOCK_CLIP`]), so
/// seeking and the position can be tried. A `lavfi` source cannot be used for them as it is: mpv
/// cannot seek it backwards and does not know its length (checked 2026-11-12 with
/// `crates/player/examples/video_probe.rs`). So the first video asked for writes the picture to a
/// file in the temp folder with libmpv's encoder (about 5 s for the video), and later runs reuse
/// it.
pub struct MockResolver;

impl htpc_resolver::StreamResolver for MockResolver {
    async fn resolve(
        &self,
        target: &htpc_resolver::Target,
        quality: &str,
    ) -> Result<htpc_resolver::PlaylistUrl, htpc_resolver::ResolveError> {
        use htpc_resolver::{PlaylistUrl, ResolveError, Target};
        tokio::time::sleep(MOCK_RESOLVE).await;
        let file = match target {
            Target::Channel { login, .. } => {
                if MOCK_OFFLINE.contains(&login.as_str()) {
                    return Err(ResolveError::Offline);
                }
                return Ok(PlaylistUrl(mock_stream_url(login, quality)));
            }
            Target::TwitchVideo(_) => &MOCK_VIDEO,
            Target::TwitchClip(_) => &MOCK_CLIP,
        };
        mock_file(file).await.map(PlaylistUrl)
    }
}

/// A generated file `--mock` plays for a video or clip.
pub struct MockFile {
    /// Its name in the temp folder. A new version of the picture needs a new name.
    name: &'static str,
    length_s: u32,
    /// The tone's pitch, so a clip sounds unlike a video.
    pitch: u32,
}

/// What every `--mock` video plays: ten minutes, long enough to scrub through.
pub const MOCK_VIDEO: MockFile = MockFile {
    name: "livedeck-mock-video-600s-v1.mkv",
    length_s: 600,
    pitch: 330,
};

/// What every `--mock` clip plays.
pub const MOCK_CLIP: MockFile = MockFile {
    name: "livedeck-mock-clip-30s-v1.mkv",
    length_s: 30,
    pitch: 550,
};

impl MockFile {
    /// The `lavfi` graph the file is made from: a small, moving test picture with a quiet tone.
    /// The tone's low sample rate keeps the encoding quick (48 kHz took 13 s for the video, 8 kHz
    /// takes 5 s).
    fn source(&self) -> String {
        let (len, pitch) = (self.length_s, self.pitch);
        format!(
            "av://lavfi:testsrc2=size=384x216:rate=15:duration={len},format=yuv420p[out0];sine=frequency={pitch}:sample_rate=8000:duration={len},volume=0.12[out1]"
        )
    }
}

/// The path of `file`, written first if this is its first use on this machine. Only one is
/// written at a time, so two plays at once do not both encode it.
async fn mock_file(file: &'static MockFile) -> Result<String, htpc_resolver::ResolveError> {
    static WRITING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let path = std::env::temp_dir().join(file.name);
    let _writing = WRITING.lock().await;
    if !path.exists() {
        tracing::info!(path = %path.display(), "writing a mock video, once");
        let source = file.source();
        let target = path.clone();
        tokio::task::spawn_blocking(move || encode_mock(&source, &target))
            .await
            .map_err(|e| e.to_string())
            .and_then(|done| done)
            .map_err(|e| {
                htpc_resolver::ResolveError::Failed(format!("the mock video failed: {e}"))
            })?;
    }
    Ok(path.to_string_lossy().into_owned())
}

/// Encodes `source` to `path` through a file beside it, so a half-written file is never used.
#[cfg(feature = "mpv")]
fn encode_mock(source: &str, path: &std::path::Path) -> Result<(), String> {
    let partial = path.with_extension("partial.mkv");
    htpc_player::encode_file(source, &partial)?;
    std::fs::rename(&partial, path).map_err(|e| e.to_string())
}

/// Without libmpv nothing can play the file anyway, so it is not made.
#[cfg(not(feature = "mpv"))]
fn encode_mock(_source: &str, _path: &std::path::Path) -> Result<(), String> {
    Err("this build has no video support".into())
}

/// The `lavfi` graph a mock channel plays at `quality` (a Streamlink name or fallback list).
fn mock_stream_url(channel: &str, quality: &str) -> String {
    // A stable pick per channel (std's hasher is seeded per run).
    let seed = channel
        .bytes()
        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    let (source, options) = MOCK_PATTERNS[seed as usize % MOCK_PATTERNS.len()];
    let pitch = 220 + (seed % 9) * 55;
    let (height, fps) = mock_rendition(quality);
    // 16:9, with an even width as the decoders' pixel formats want.
    let width = (height * 16 / 9 + 1) & !1;
    // `format` drops the alpha some sources have, which mpv would draw over a checkerboard.
    format!(
        "av://lavfi:{source}=size={width}x{height}:rate={fps}{options},format=yuv420p[out0];sine=frequency={pitch}:sample_rate=48000,volume=0.12[out1]"
    )
}

/// The height and frame rate the first name of a Streamlink list stands for: `480p30` is 480 lines
/// at 30 fps, `best` 1080p60, `worst` 160p30. Sound only gets a small picture, which mpv does not
/// decode then.
fn mock_rendition(quality: &str) -> (u32, u32) {
    let first = quality.split(',').next().unwrap_or_default();
    match first {
        "best" => (1080, 60),
        "worst" | "audio_only" => (160, 30),
        name => {
            let (lines, rate) = name.split_once('p').unwrap_or((name, ""));
            let height = lines.parse().unwrap_or(720);
            let fps = if rate == "60" { 60 } else { 30 };
            (height, fps)
        }
    }
}

/// Kick's side of `--mock`: two pages of Top Live and a channel that is not live, so the Kick row
/// pages and Search finds both kinds.
pub fn fake_kick() -> FakeKick {
    let names = [
        "neon_drift",
        "gravel-king",
        "moth_radio",
        "quiet-harbor",
        "copperfox",
        "lumen_bay",
        "tidewalker",
        "saltmarsh",
        "night-owl-fm",
        "brickworks",
        "pale_ember",
        "slow-river",
    ];
    let streams: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(i, name)| fake_live(name, 9_000 - i as u64 * 650))
        .collect();
    FakeKick::new()
        .with_pages(streams.chunks(8).map(<[_]>::to_vec).collect())
        .with_offline("sleepy-fox", "SleepyFox")
}

#[cfg(test)]
mod tests {
    use super::*;
    use htpc_resolver::{ResolveError, Site, StreamResolver, Target};

    #[test]
    fn the_picture_is_as_big_as_the_rendition_asked_for() {
        assert_eq!(mock_rendition("480p30,480p,480p60,360p30,worst"), (480, 30));
        assert_eq!(mock_rendition("720p60,720p,720p30,worst"), (720, 60));
        assert_eq!(mock_rendition("160p"), (160, 30));
        assert_eq!(mock_rendition("best"), (1080, 60));
        assert_eq!(mock_rendition("worst"), (160, 30));
        assert_eq!(mock_rendition("audio_only"), (160, 30));
    }

    #[test]
    fn each_channel_keeps_its_own_picture_and_a_16_by_9_size() {
        let a = mock_stream_url("marrow", "360p30,worst");
        assert_eq!(a, mock_stream_url("marrow", "360p30,worst"));
        assert!(a.starts_with("av://lavfi:"), "{a}");
        assert!(a.contains("size=640x360:rate=30"), "{a}");
        assert!(a.contains("[out0];sine=") && a.ends_with("[out1]"), "{a}");
        let big = mock_stream_url("marrow", "480p30");
        assert!(big.contains("size=854x480"), "{big}");
    }

    #[tokio::test(start_paused = true)]
    async fn offline_mock_channels_stay_offline() {
        let offline = MockResolver
            .resolve(&Target::channel(Site::Twitch, "driftwood"), "best")
            .await;
        assert_eq!(offline, Err(ResolveError::Offline));
        let live = MockResolver
            .resolve(&Target::channel(Site::Kick, "neon_drift"), "best")
            .await;
        assert!(live.is_ok_and(|url| url.0.contains("1920x1080")));
    }

    /// Writes the mock clip with libmpv (or reuses it) and checks the answer is that file.
    #[cfg(feature = "mpv")]
    #[tokio::test]
    #[ignore = "needs libmpv; writes a file in the temp folder"]
    async fn a_mock_clip_is_written_once_and_played_from_its_file() {
        let clip = Target::TwitchClip("AnyClip".into());
        let url = MockResolver.resolve(&clip, "best").await.expect("written");
        assert!(url.0.ends_with(MOCK_CLIP.name), "{}", url.0);
        assert!(std::fs::metadata(&url.0).is_ok_and(|m| m.len() > 0));
        let again = MockResolver.resolve(&clip, "best").await;
        assert_eq!(again, Ok(url));
    }

    #[test]
    fn mock_videos_are_made_from_a_picture_of_their_length() {
        let video = MOCK_VIDEO.source();
        assert!(video.starts_with("av://lavfi:testsrc2="), "{video}");
        assert!(
            video.contains(":duration=600,") && video.ends_with("[out1]"),
            "{video}"
        );
        assert!(MOCK_CLIP.source().contains(":duration=30,"));
        assert_ne!(MOCK_VIDEO.name, MOCK_CLIP.name);
    }
}
