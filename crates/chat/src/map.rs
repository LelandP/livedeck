//! Turns the chat server's messages into the core's chat types.

use std::sync::Arc;
use std::time::Duration;

use htpc_core::{ChatKind, ChatMessage, ChatSpan, Moderation, Rgb, RoomModeChange};
use twitch_irc::message::{
    Badge, ClearChatAction, ClearChatMessage, ClearMsgMessage, Emote, FollowersOnlyMode,
    NoticeMessage, PrivmsgMessage, RGBColor, RoomStateMessage, ServerMessage, UserNoticeMessage,
};

use crate::badges::BadgeBook;
use crate::emotes::EmoteProvider;

/// What one server message means for the lines on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mapped {
    /// A line to add.
    Line(ChatMessage),
    /// Lines to remove.
    Moderation(Moderation),
    /// The room's rules for who may write (slow mode, followers only, ...) changed.
    RoomMode(RoomModeChange),
}

/// Maps the messages of one channel. Messages for any other channel are ignored.
pub struct Mapper {
    channel: String,
    emotes: Arc<dyn EmoteProvider>,
    badges: Arc<BadgeBook>,
}

impl Mapper {
    pub fn new(channel: &str, emotes: Arc<dyn EmoteProvider>, badges: Arc<BadgeBook>) -> Self {
        Self {
            channel: channel.to_ascii_lowercase(),
            emotes,
            badges,
        }
    }

    fn is_ours(&self, login: &str) -> bool {
        login.eq_ignore_ascii_case(&self.channel)
    }

    /// The effect of `message`: usually one line, the room's settings for a `ROOMSTATE`, none for
    /// traffic that is not chat (pings, joins).
    pub fn map(&self, message: ServerMessage) -> Vec<Mapped> {
        match message {
            ServerMessage::Privmsg(m) if self.is_ours(&m.channel_login) => {
                vec![Mapped::Line(self.privmsg(m))]
            }
            ServerMessage::UserNotice(m) if self.is_ours(&m.channel_login) => self.usernotice(m),
            // Notices before login have no channel, and say nothing a viewer needs.
            ServerMessage::Notice(NoticeMessage {
                channel_login: Some(channel),
                message_text,
                ..
            }) if self.is_ours(&channel) && !message_text.trim().is_empty() => {
                vec![Mapped::Line(ChatMessage::notice(message_text.trim()))]
            }
            ServerMessage::ClearChat(m) if self.is_ours(&m.channel_login) => clear_chat(m),
            ServerMessage::ClearMsg(m) if self.is_ours(&m.channel_login) => clear_msg(m),
            ServerMessage::RoomState(m) if self.is_ours(&m.channel_login) => {
                vec![Mapped::RoomMode(room_mode(&m))]
            }
            _ => Vec::new(),
        }
    }

    fn privmsg(&self, m: PrivmsgMessage) -> ChatMessage {
        ChatMessage {
            id: m.message_id,
            login: m.sender.login,
            user: m.sender.name,
            color: m.name_color.map(rgb),
            badges: self.badge_urls(&m.badges),
            spans: build_spans(&m.message_text, &m.emotes, self.emotes.as_ref()),
            kind: if m.is_action {
                ChatKind::Action
            } else {
                ChatKind::Message
            },
        }
    }

    /// Subscriptions, raids and the like: Twitch's own sentence as a notice, then the chatter's
    /// message if they attached one.
    fn usernotice(&self, m: UserNoticeMessage) -> Vec<Mapped> {
        let mut out = Vec::new();
        let system = m.system_message.trim();
        if !system.is_empty() {
            out.push(Mapped::Line(ChatMessage::notice(system)));
        }
        if let Some(text) = m.message_text.as_deref().filter(|t| !t.trim().is_empty()) {
            out.push(Mapped::Line(ChatMessage {
                id: m.message_id,
                login: m.sender.login,
                user: m.sender.name,
                color: m.name_color.map(rgb),
                badges: self.badge_urls(&m.badges),
                spans: build_spans(text, &m.emotes, self.emotes.as_ref()),
                kind: ChatKind::Message,
            }));
        }
        out
    }

    fn badge_urls(&self, badges: &[Badge]) -> Vec<String> {
        badges
            .iter()
            .filter_map(|b| self.badges.url(&b.name, &b.version))
            .collect()
    }
}

fn rgb(c: RGBColor) -> Rgb {
    Rgb::new(c.r, c.g, c.b)
}

fn clear_chat(m: ClearChatMessage) -> Vec<Mapped> {
    let action = match m.action {
        ClearChatAction::ChatCleared => Moderation::ClearAll,
        ClearChatAction::UserBanned { user_login, .. }
        | ClearChatAction::UserTimedOut { user_login, .. } => Moderation::ClearUser(user_login),
    };
    vec![Mapped::Moderation(action)]
}

/// The settings a `ROOMSTATE` names. Joining sends all of them, a change only its own.
fn room_mode(m: &RoomStateMessage) -> RoomModeChange {
    let secs = |d: Duration| u32::try_from(d.as_secs()).unwrap_or(u32::MAX);
    RoomModeChange {
        slow_secs: m.slow_mode.map(secs),
        followers_min: m.followers_only.as_ref().map(|f| match f {
            FollowersOnlyMode::Disabled => None,
            FollowersOnlyMode::Enabled(d) => Some(secs(*d) / 60),
        }),
        subs_only: m.subscribers_only,
        emote_only: m.emote_only,
    }
}

fn clear_msg(m: ClearMsgMessage) -> Vec<Mapped> {
    vec![Mapped::Moderation(Moderation::Delete(m.message_id))]
}

/// Splits `text` into text and emote spans. `tagged` emotes (character ranges from the server) are
/// cut out first. Whatever remains is checked word by word for third-party emotes.
pub fn build_spans(text: &str, tagged: &[Emote], provider: &dyn EmoteProvider) -> Vec<ChatSpan> {
    let chars: Vec<char> = text.chars().collect();
    let mut order: Vec<&Emote> = tagged.iter().collect();
    order.sort_by_key(|e| e.char_range.start);

    let mut spans = Vec::new();
    let mut cursor = 0;
    for emote in order {
        // Twitch has been known to send ranges past the end of the text.
        let start = emote.char_range.start;
        let end = emote.char_range.end.min(chars.len());
        if start < cursor || start >= end {
            continue;
        }
        push_words(&mut spans, &chars[cursor..start], provider);
        match provider.tagged(&emote.id) {
            Some(url) => spans.push(ChatSpan::Emote {
                name: emote.code.clone(),
                url,
            }),
            // No image for it: it stays text.
            None => push_words(&mut spans, &chars[start..end], provider),
        }
        cursor = end;
    }
    push_words(&mut spans, &chars[cursor.min(chars.len())..], provider);
    merge_text(spans)
}

/// Joins text spans that ended up next to each other (an emote that stayed text).
fn merge_text(spans: Vec<ChatSpan>) -> Vec<ChatSpan> {
    let mut out: Vec<ChatSpan> = Vec::with_capacity(spans.len());
    for span in spans {
        match (out.last_mut(), span) {
            (Some(ChatSpan::Text(prev)), ChatSpan::Text(next)) => {
                prev.push(' ');
                prev.push_str(&next);
            }
            (_, span) => out.push(span),
        }
    }
    out
}

/// Adds `chars` as text, pulling out the words the provider knows as emotes.
fn push_words(spans: &mut Vec<ChatSpan>, chars: &[char], provider: &dyn EmoteProvider) {
    let text: String = chars.iter().collect();
    let mut plain: Vec<&str> = Vec::new();
    let flush = |spans: &mut Vec<ChatSpan>, plain: &mut Vec<&str>| {
        if !plain.is_empty() {
            spans.push(ChatSpan::Text(plain.join(" ")));
            plain.clear();
        }
    };
    for word in text.split_whitespace() {
        match provider.word(word) {
            Some(url) => {
                flush(spans, &mut plain);
                spans.push(ChatSpan::Emote {
                    name: word.to_owned(),
                    url,
                });
            }
            None => plain.push(word),
        }
    }
    flush(spans, &mut plain);
}
