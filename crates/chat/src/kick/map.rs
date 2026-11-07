//! Kick's Pusher frames to the core's chat types. Pure, so it is tested on recorded frames.
//!
//! A frame is `{"event": "...", "data": "<JSON as a string>", "channel": "..."}`. The events read:
//! - `App\Events\ChatMessageEvent`: a line (`type` `message` or `reply`).
//! - `App\Events\MessageDeletedEvent`: one line removed (`message.id`).
//! - `App\Events\UserBannedEvent`: a chatter banned or timed out (`user.slug`).
//! - `App\Events\ChatroomClearEvent`: the whole chat cleared.
//!
//! Emotes are written inline as `[emote:<id>:<name>]`.

use htpc_core::{ChatKind, ChatMessage, ChatSpan, Moderation, Rgb};
use serde::Deserialize;

/// Kick's emote images, by id.
pub fn emote_url(id: &str) -> String {
    format!("https://files.kick.com/emotes/{id}/fullsize")
}

/// What one frame means to the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// The socket is open (Pusher's `connection_established`).
    Connected,
    /// The chatroom subscription took.
    Subscribed,
    /// The server wants a `pusher:pong`.
    Ping,
    /// The answer to our own ping.
    Pong,
    Message(ChatMessage),
    Moderation(Moderation),
    /// Pusher refused something (bad app key, ...), with its message. The connection is useless.
    Error(String),
    /// Anything else (pins, polls, gifts, ...).
    Other,
}

#[derive(Deserialize)]
struct Envelope {
    event: String,
    /// Pusher sends the payload as a JSON string; its own events sometimes as an object.
    #[serde(default)]
    data: serde_json::Value,
}

#[derive(Deserialize)]
struct Line {
    #[serde(default)]
    id: String,
    #[serde(default)]
    content: String,
    sender: Sender,
}

#[derive(Deserialize)]
struct Sender {
    #[serde(default)]
    username: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    identity: Identity,
}

#[derive(Deserialize, Default)]
struct Identity {
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    badges_v2: Vec<Badge>,
}

#[derive(Deserialize)]
struct Badge {
    #[serde(default)]
    image_url: Option<String>,
    #[serde(default)]
    sort_order: i64,
}

#[derive(Deserialize)]
struct Deleted {
    message: Ref,
}

#[derive(Deserialize)]
struct Banned {
    user: Ref,
}

#[derive(Deserialize)]
struct Ref {
    #[serde(default)]
    id: serde_json::Value,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    username: Option<String>,
}

/// The payload as a JSON value, whether it came as a string or an object.
fn payload(data: serde_json::Value) -> serde_json::Value {
    match data {
        serde_json::Value::String(s) => serde_json::from_str(&s).unwrap_or(serde_json::Value::Null),
        other => other,
    }
}

/// Reads one frame. A frame that cannot be read is `Other`: one odd line is no reason to drop the
/// connection.
pub fn read_frame(text: &str) -> Frame {
    let Ok(env) = serde_json::from_str::<Envelope>(text) else {
        return Frame::Other;
    };
    let data = || payload(env.data.clone());
    match env.event.as_str() {
        "pusher:connection_established" => Frame::Connected,
        "pusher_internal:subscription_succeeded" => Frame::Subscribed,
        "pusher:ping" => Frame::Ping,
        "pusher:pong" => Frame::Pong,
        "pusher:error" => {
            let message = data()
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Pusher error")
                .to_owned();
            Frame::Error(message)
        }
        r"App\Events\ChatMessageEvent" => serde_json::from_value::<Line>(data())
            .map(|l| Frame::Message(line(l)))
            .unwrap_or(Frame::Other),
        r"App\Events\MessageDeletedEvent" => serde_json::from_value::<Deleted>(data())
            .ok()
            .and_then(|d| id_text(&d.message.id))
            .map(|id| Frame::Moderation(Moderation::Delete(id)))
            .unwrap_or(Frame::Other),
        r"App\Events\UserBannedEvent" => serde_json::from_value::<Banned>(data())
            .ok()
            .and_then(|b| b.user.slug.or(b.user.username))
            .map(|who| Frame::Moderation(Moderation::ClearUser(who.to_ascii_lowercase())))
            .unwrap_or(Frame::Other),
        r"App\Events\ChatroomClearEvent" => Frame::Moderation(Moderation::ClearAll),
        _ => Frame::Other,
    }
}

fn id_text(id: &serde_json::Value) -> Option<String> {
    match id {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn line(l: Line) -> ChatMessage {
    let mut badges: Vec<&Badge> = l.sender.identity.badges_v2.iter().collect();
    badges.sort_by_key(|b| b.sort_order);
    let login = if l.sender.slug.is_empty() {
        l.sender.username.to_ascii_lowercase()
    } else {
        l.sender.slug.to_ascii_lowercase()
    };
    ChatMessage {
        id: l.id,
        login,
        user: l.sender.username,
        color: l.sender.identity.color.as_deref().and_then(parse_color),
        badges: badges
            .into_iter()
            .filter_map(|b| b.image_url.clone())
            .collect(),
        spans: spans(&l.content),
        kind: ChatKind::Message,
    }
}

/// `#RRGGBB`.
fn parse_color(hex: &str) -> Option<Rgb> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    u32::from_str_radix(hex, 16).ok().map(Rgb::from_hex)
}

/// The text of a line as spans: runs of words, and each `[emote:<id>:<name>]` as an emote.
pub fn spans(content: &str) -> Vec<ChatSpan> {
    let mut out = Vec::new();
    let mut words: Vec<&str> = Vec::new();
    let flush = |out: &mut Vec<ChatSpan>, words: &mut Vec<&str>| {
        if !words.is_empty() {
            out.push(ChatSpan::Text(words.join(" ")));
            words.clear();
        }
    };
    // Tags hold no spaces, so they are found inside words (also glued to text: `hi[emote:1:x]`).
    for word in content.split_whitespace() {
        let mut rest = word;
        while let Some((start, len, id, name)) = find_emote(rest) {
            if start > 0 {
                words.push(&rest[..start]);
            }
            flush(&mut out, &mut words);
            out.push(ChatSpan::Emote {
                url: emote_url(id),
                name: name.to_owned(),
            });
            rest = &rest[start + len..];
        }
        if !rest.is_empty() {
            words.push(rest);
        }
    }
    flush(&mut out, &mut words);
    out
}

/// The first well-formed `[emote:<digits>:<name>]` in `text`: where it starts, its length, the id
/// and the name.
fn find_emote(text: &str) -> Option<(usize, usize, &str, &str)> {
    const TAG: &str = "[emote:";
    let mut from = 0;
    while let Some(found) = text[from..].find(TAG) {
        let start = from + found;
        let body = &text[start + TAG.len()..];
        if let Some(end) = body.find(']') {
            if let Some((id, name)) = body[..end].split_once(':') {
                if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) && !name.is_empty() {
                    return Some((start, TAG.len() + end + 1, id, name));
                }
            }
        }
        from = start + TAG.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emotes_are_cut_out_of_the_text() {
        assert_eq!(
            spans("hi [emote:37226:KEKW] there [emote:5:x]"),
            vec![
                ChatSpan::Text("hi".into()),
                ChatSpan::Emote {
                    name: "KEKW".into(),
                    url: emote_url("37226")
                },
                ChatSpan::Text("there".into()),
                ChatSpan::Emote {
                    name: "x".into(),
                    url: emote_url("5")
                },
            ]
        );
    }

    #[test]
    fn broken_emote_tags_stay_text() {
        assert_eq!(
            spans("a [emote:abc:x] b [emote:12"),
            vec![ChatSpan::Text("a [emote:abc:x] b [emote:12".into())]
        );
        assert!(spans("   ").is_empty());
    }

    #[test]
    fn an_emote_glued_to_text_is_still_an_emote() {
        assert_eq!(
            spans("lol[emote:7:pog]!"),
            vec![
                ChatSpan::Text("lol".into()),
                ChatSpan::Emote {
                    name: "pog".into(),
                    url: emote_url("7")
                },
                ChatSpan::Text("!".into()),
            ]
        );
    }

    #[test]
    fn colours_are_hex() {
        assert_eq!(parse_color("#FFA600"), Some(Rgb::new(0xff, 0xa6, 0)));
        assert_eq!(parse_color("red"), None);
    }

    #[test]
    fn pusher_control_frames() {
        assert_eq!(
            read_frame(r#"{"event":"pusher:ping","data":{}}"#),
            Frame::Ping
        );
        assert_eq!(
            read_frame(
                r#"{"event":"pusher:error","data":{"code":4001,"message":"App key x not in this cluster"}}"#
            ),
            Frame::Error("App key x not in this cluster".into())
        );
        assert_eq!(read_frame("not json"), Frame::Other);
    }

    #[test]
    fn moderation_frames() {
        let deleted = r#"{"event":"App\\Events\\MessageDeletedEvent","data":"{\"id\":\"d1\",\"message\":{\"id\":\"m-9\"}}","channel":"chatrooms.1.v2"}"#;
        assert_eq!(
            read_frame(deleted),
            Frame::Moderation(Moderation::Delete("m-9".into()))
        );
        let banned = r#"{"event":"App\\Events\\UserBannedEvent","data":"{\"user\":{\"id\":5,\"username\":\"Some-One\",\"slug\":\"some-one\"}}","channel":"chatrooms.1.v2"}"#;
        assert_eq!(
            read_frame(banned),
            Frame::Moderation(Moderation::ClearUser("some-one".into()))
        );
        let cleared = r#"{"event":"App\\Events\\ChatroomClearEvent","data":"{\"id\":\"c\"}","channel":"chatrooms.1.v2"}"#;
        assert_eq!(read_frame(cleared), Frame::Moderation(Moderation::ClearAll));
    }
}
