//! One WebSocket to Pusher, as a pair of channels of text frames. The session only sees a
//! [`FrameLink`], so tests drive it with scripted ones.

use std::future::Future;
use std::pin::Pin;

use async_tungstenite::tungstenite::Message;
use futures_util::StreamExt;
use tokio::sync::mpsc;

/// Frames queued from the socket before the session reads them.
const INCOMING_BUFFER: usize = 256;

/// An open socket. Dropping it closes the socket.
pub struct FrameLink {
    /// Text frames from the server. `Err` is fatal: the socket is gone.
    pub incoming: mpsc::Receiver<Result<String, String>>,
    pub outgoing: mpsc::UnboundedSender<String>,
}

pub type FrameFuture<'a> = Pin<Box<dyn Future<Output = Result<FrameLink, String>> + Send + 'a>>;

/// Opens sockets.
pub trait FrameConnector: Send + Sync + 'static {
    fn connect<'a>(&'a self, url: &'a str) -> FrameFuture<'a>;
}

/// The real thing: a secure WebSocket.
#[derive(Debug, Clone, Copy, Default)]
pub struct WsFrames;

impl FrameConnector for WsFrames {
    fn connect<'a>(&'a self, url: &'a str) -> FrameFuture<'a> {
        Box::pin(async move {
            let (ws, _) = async_tungstenite::tokio::connect_async(url)
                .await
                .map_err(|e| format!("could not connect to Kick chat: {e}"))?;
            let (mut sink, mut stream) = ws.split();
            let (in_tx, in_rx) = mpsc::channel(INCOMING_BUFFER);
            let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
            tokio::spawn(async move {
                while let Some(item) = stream.next().await {
                    let item = match item {
                        Ok(Message::Text(text)) => Ok(text.as_str().to_owned()),
                        Ok(Message::Close(frame)) => Err(format!("closed by Kick: {frame:?}")),
                        Ok(_) => continue,
                        Err(e) => Err(e.to_string()),
                    };
                    let fatal = item.is_err();
                    if in_tx.send(item).await.is_err() || fatal {
                        break;
                    }
                }
            });
            tokio::spawn(async move {
                while let Some(text) = out_rx.recv().await {
                    if sink.send(Message::text(text)).await.is_err() {
                        break;
                    }
                }
            });
            Ok(FrameLink {
                incoming: in_rx,
                outgoing: out_tx,
            })
        })
    }
}
