//! Thin WebSocket client for the LocalSync signaling relay
//! (`apps/signaling-server`). Carries only SDP offer/answer JSON - never
//! payload bytes, which travel exclusively over the WebRTC data channel.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

/// Messages exchanged with the signaling server. Only SDP is used here:
/// ICE candidates are not trickled separately (see lib.rs doc comment on
/// non-trickle ICE) but the server itself is agnostic to message shape, it
/// just relays whatever JSON it's given.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub(crate) enum SignalMsg {
    Offer { data: RTCSessionDescription },
    Answer { data: RTCSessionDescription },
}

#[derive(Clone)]
pub(crate) struct SignalingClient {
    outbound_tx: mpsc::UnboundedSender<SignalMsg>,
}

impl SignalingClient {
    /// Connects to `<signaling_url>/<room_code>` and spawns background
    /// tasks to pump messages in both directions. Returns a handle for
    /// sending, plus a channel of messages received from the peer.
    pub async fn connect(
        signaling_url: &str,
        room_code: &str,
    ) -> Result<(Self, mpsc::UnboundedReceiver<SignalMsg>)> {
        let url = format!("{}/{}", signaling_url.trim_end_matches('/'), room_code);
        let (ws_stream, _) = tokio_tungstenite::connect_async(&url)
            .await
            .with_context(|| format!("failed to connect to signaling server at {url}"))?;
        let (mut write, mut read) = ws_stream.split();

        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<SignalMsg>();
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel::<SignalMsg>();

        // Writer: drain outgoing signaling messages onto the socket.
        tokio::spawn(async move {
            while let Some(msg) = outbound_rx.recv().await {
                let Ok(text) = serde_json::to_string(&msg) else {
                    continue;
                };
                if write.send(WsMessage::Text(text.into())).await.is_err() {
                    break;
                }
            }
        });

        // Reader: forward parseable signaling messages to the caller.
        // Anything else (e.g. {"type":"peer-disconnected"}) is ignored -
        // this MVP only needs the one-shot offer/answer exchange.
        tokio::spawn(async move {
            while let Some(Ok(msg)) = read.next().await {
                if let WsMessage::Text(text) = msg {
                    if let Ok(sig) = serde_json::from_str::<SignalMsg>(&text) {
                        if inbound_tx.send(sig).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Ok((Self { outbound_tx }, inbound_rx))
    }

    pub fn send(&self, msg: SignalMsg) -> Result<()> {
        self.outbound_tx
            .send(msg)
            .map_err(|_| anyhow::anyhow!("signaling connection closed"))
    }
}

/// Waits for the next Offer or Answer, ignoring anything else.
pub(crate) async fn recv_sdp(
    rx: &mut mpsc::UnboundedReceiver<SignalMsg>,
) -> Result<RTCSessionDescription> {
    loop {
        match rx.recv().await {
            Some(SignalMsg::Offer { data }) | Some(SignalMsg::Answer { data }) => return Ok(data),
            None => anyhow::bail!("signaling connection closed before SDP was received"),
        }
    }
}
