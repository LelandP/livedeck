//! `--mock` fixture data: the scaffold's rows and chat, served through `htpc_twitch_api::FakeApi`
//! so mock mode exercises the same `ApiProvider` mapping and `Event`s as the real Helix client.

use std::time::Duration;

use htpc_chat::{ChatEvent, FakeChat};
use htpc_core::{ChatKind, ChatMessage, ChatSpan, ChatStatus, Rgb};
use htpc_twitch_api::{FakeApi, FollowedChannel, Game, StreamInfo, UserInfo};

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
