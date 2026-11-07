//! Chat commands: join the channel on screen, leave it again. What the chat client reports comes
//! back to the store as core `Event`s.

use std::sync::{Arc, Mutex};

use htpc_chat::{ChatClient, ChatEvent, ChatSession};
use htpc_core::{ChannelRef, Command, Event, Msg};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

/// Holds the one chat connection of the channel being watched.
pub struct Chat {
    client: Arc<dyn ChatClient>,
    tx: UnboundedSender<Msg>,
    /// Dropping the session leaves the channel.
    session: Mutex<Option<ChatSession>>,
}

impl Chat {
    /// `tx` is the sender of the store's channel, which the events go into.
    pub fn new(client: Arc<dyn ChatClient>, tx: UnboundedSender<Msg>) -> Self {
        Self {
            client,
            tx,
            session: Mutex::new(None),
        }
    }

    /// True for the commands this module runs.
    pub fn handles(cmd: &Command) -> bool {
        matches!(cmd, Command::JoinChat { .. } | Command::LeaveChat)
    }

    /// Runs a chat command. Not async, and run by the store itself rather than in a task of its
    /// own, so that a leave followed by a join happens in that order. Must be called inside the
    /// tokio runtime.
    pub fn execute(&self, cmd: Command) {
        match cmd {
            Command::JoinChat { channel } => {
                let (events_tx, mut events) = unbounded_channel();
                let session = self.client.join(&channel.login, events_tx);
                // Replacing the old session drops it, which leaves its channel.
                *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session);
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    while let Some(event) = events.recv().await {
                        if tx.send(to_event(&channel, event).into()).is_err() {
                            break;
                        }
                    }
                });
            }
            Command::LeaveChat => {
                self.session
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
            }
            _ => {}
        }
    }
}

fn to_event(channel: &ChannelRef, event: ChatEvent) -> Event {
    let channel = channel.clone();
    match event {
        ChatEvent::Status(status) => Event::ChatStatus { channel, status },
        ChatEvent::Messages(messages) => Event::ChatMessages { channel, messages },
        ChatEvent::Moderated(action) => Event::ChatModerated { channel, action },
        ChatEvent::RoomMode(change) => Event::ChatRoomMode { channel, change },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use htpc_chat::FakeChat;
    use htpc_core::{ChatMessage, ChatStatus, Moderation};
    use std::time::Duration;
    use tokio::sync::mpsc::unbounded_channel;

    fn join(channel: &str) -> Command {
        Command::JoinChat {
            channel: ChannelRef::twitch(channel),
        }
    }

    fn script() -> Vec<ChatEvent> {
        vec![
            ChatEvent::Status(ChatStatus::Live),
            ChatEvent::Messages(vec![ChatMessage::plain("a", "hi")]),
            ChatEvent::Moderated(Moderation::ClearAll),
        ]
    }

    async fn settle() {
        // Lets the aborted and spawned tasks run.
        for _ in 0..5 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn what_the_client_reports_becomes_events_for_the_channel() {
        let (tx, mut rx) = unbounded_channel();
        let chat = Chat::new(Arc::new(FakeChat::new(script())), tx);
        chat.execute(join("chan"));

        let mut got = Vec::new();
        for _ in 0..3 {
            match rx.recv().await.unwrap() {
                Msg::Event(e) => got.push(e),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(
            got[0],
            Event::ChatStatus {
                channel: ChannelRef::twitch("chan"),
                status: ChatStatus::Live
            }
        );
        assert!(matches!(&got[1], Event::ChatMessages { channel, messages }
            if channel.login == "chan" && messages.len() == 1));
        assert_eq!(
            got[2],
            Event::ChatModerated {
                channel: ChannelRef::twitch("chan"),
                action: Moderation::ClearAll
            }
        );
    }

    #[tokio::test]
    async fn leaving_and_switching_close_the_connection() {
        let (tx, _rx) = unbounded_channel();
        let fake = FakeChat::new(Vec::new());
        let chat = Chat::new(Arc::new(fake.clone()), tx);

        chat.execute(join("one"));
        settle().await;
        assert_eq!(fake.sessions_alive(), 1);

        // Joining another channel replaces the first, never two at once.
        chat.execute(join("two"));
        settle().await;
        assert_eq!(fake.sessions_alive(), 1);
        assert_eq!(fake.joined(), ["one", "two"]);

        chat.execute(Command::LeaveChat);
        settle().await;
        assert_eq!(fake.sessions_alive(), 0);
        // Leaving twice is harmless.
        chat.execute(Command::LeaveChat);
    }

    #[test]
    fn handles_only_join_and_leave() {
        assert!(Chat::handles(&join("x")));
        assert!(Chat::handles(&Command::LeaveChat));
        assert!(!Chat::handles(&Command::StopPlayback));
        assert!(!Chat::handles(&Command::FetchChatImages {
            urls: Vec::new()
        }));
    }
}
