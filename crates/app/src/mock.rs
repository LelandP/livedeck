//! `--mock` fixture data: the scaffold's rows and chat, served through `htpc_twitch_api::FakeApi`
//! so mock mode exercises the same `ApiProvider` mapping and `Event`s as the real Helix client.

use std::time::Duration;

use htpc_chat::{ChatEvent, FakeChat};
use htpc_core::{ChatKind, ChatMessage, ChatSpan, ChatStatus, Rgb};
use htpc_kick_api::{fake_live, FakeKick};
use htpc_twitch_api::{FakeApi, FollowedChannel, Game, HypeTrain, StreamInfo, UserInfo};

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
        // Messages are "posted" and kept, but nothing echoes them back into the mock chat.
        ..FakeApi::new()
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
pub struct MockResolver;

impl htpc_resolver::StreamResolver for MockResolver {
    async fn resolve(
        &self,
        _site: htpc_resolver::Site,
        channel: &str,
        quality: &str,
    ) -> Result<htpc_resolver::PlaylistUrl, htpc_resolver::ResolveError> {
        tokio::time::sleep(MOCK_RESOLVE).await;
        if MOCK_OFFLINE.contains(&channel) {
            return Err(htpc_resolver::ResolveError::Offline);
        }
        Ok(htpc_resolver::PlaylistUrl(mock_stream_url(
            channel, quality,
        )))
    }
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
    use htpc_resolver::{ResolveError, Site, StreamResolver};

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
            .resolve(Site::Twitch, "driftwood", "best")
            .await;
        assert_eq!(offline, Err(ResolveError::Offline));
        let live = MockResolver.resolve(Site::Kick, "neon_drift", "best").await;
        assert!(live.is_ok_and(|url| url.0.contains("1920x1080")));
    }
}
