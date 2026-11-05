//! One connection to the chat server, as a pair of channels.
//!
//! The session loop only sees a [`Link`], so tests drive it with scripted ones and the real
//! [`WsConnector`] is the only code that touches a socket.

use std::future::Future;
use std::pin::Pin;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use twitch_irc::message::IRCMessage;
use twitch_irc::transport::Transport;
use twitch_irc::SecureWSTransport;

/// Messages queued from the socket before the session loop reads them.
const INCOMING_BUFFER: usize = 256;

/// An open connection. Dropping it closes the socket.
pub struct Link {
    /// What the server sent. `Err` is fatal: the connection is gone.
    pub incoming: mpsc::Receiver<Result<IRCMessage, String>>,
    pub outgoing: mpsc::UnboundedSender<IRCMessage>,
}

pub type ConnectFuture<'a> = Pin<Box<dyn Future<Output = Result<Link, String>> + Send + 'a>>;

/// Opens connections to the chat server.
pub trait Connector: Send + Sync + 'static {
    fn connect(&self) -> ConnectFuture<'_>;
}

/// Twitch's chat server over a secure WebSocket (`wss://irc-ws.chat.twitch.tv`).
#[derive(Debug, Clone, Copy, Default)]
pub struct WsConnector;

impl Connector for WsConnector {
    fn connect(&self) -> ConnectFuture<'_> {
        Box::pin(async {
            let transport = SecureWSTransport::new()
                .await
                .map_err(|e| format!("could not connect to chat: {e}"))?;
            let (mut incoming, mut outgoing) = transport.split();
            let (in_tx, in_rx) = mpsc::channel(INCOMING_BUFFER);
            let (out_tx, mut out_rx) = mpsc::unbounded_channel::<IRCMessage>();

            tokio::spawn(async move {
                while let Some(item) = incoming.next().await {
                    let item = match item {
                        Ok(message) => Ok(message),
                        // One line that does not parse is not worth a reconnect.
                        Err(e) if !e.is_left() => {
                            tracing::debug!(error = %e, "skipping an unparseable chat line");
                            continue;
                        }
                        Err(e) => Err(e.to_string()),
                    };
                    let fatal = item.is_err();
                    // The receiver is gone once the session dropped the link.
                    if in_tx.send(item).await.is_err() || fatal {
                        break;
                    }
                }
            });
            tokio::spawn(async move {
                while let Some(message) = out_rx.recv().await {
                    if outgoing.send(message).await.is_err() {
                        break;
                    }
                }
            });
            Ok(Link {
                incoming: in_rx,
                outgoing: out_tx,
            })
        })
    }
}
