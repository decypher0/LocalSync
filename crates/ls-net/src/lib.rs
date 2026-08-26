//! Pure transport layer for LocalSync.
//!
//! Moves opaque bytes between two peers over an encrypted WebRTC data
//! channel, using a signaling server (`apps/signaling-server`) only to
//! exchange SDP offer/answer during connection setup. This crate knows
//! nothing about snapshots, manifests, diffs, or containers - callers hand
//! it `&[u8]` and get `Vec<u8>` back.
//!
//! ## NAT traversal
//! Connections use a public STUN server (Google's) to discover reachable
//! addresses. That's enough for peers on the same LAN or behind
//! "easy" NATs, which is what the MVP demo needs. A TURN relay fallback for
//! strict/symmetric NATs is a real production requirement but is not
//! implemented yet - see the `ice_config` TODO below.
//!
//! ## ICE strategy
//! Candidates are *not* trickled over the signaling channel as they're
//! discovered. Instead each side waits for ICE gathering to complete before
//! sending its SDP, so the offer/answer already contains every candidate.
//! This means exactly one signaling round-trip per side (simpler, and
//! avoids a class of "candidate arrived before remote description was set"
//! races) at the cost of added connect latency vs. trickle ICE. Fine for
//! the MVP; revisit if connection setup time on real NATs becomes a
//! problem.

mod signaling;

use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::{APIBuilder, API};
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::RTCPeerConnection;

use signaling::{recv_sdp, SignalMsg, SignalingClient};

/// A message-size chunk. WebRTC data channels are commonly capped around
/// 16KB per message across implementations; this stays comfortably under
/// that regardless of peer.
const CHUNK_SIZE: usize = 16 * 1024;

/// How long connection setup (signaling + ICE + DTLS + data channel open)
/// is allowed to take before giving up. Doesn't apply to the payload
/// transfer itself, which has no timeout - large payloads just take longer.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// An open, encrypted P2P data channel to one peer. Opaque: callers only
/// ever see bytes in and bytes out via [`send_payload`] / [`receive_payload`].
pub struct DataChannelConn {
    dc: Arc<RTCDataChannel>,
    incoming: AsyncMutex<mpsc::UnboundedReceiver<Bytes>>,
    // Kept alive for the lifetime of the connection; dropping it tears down
    // ICE/DTLS/SCTP. Never read directly.
    _pc: Arc<RTCPeerConnection>,
}

fn build_api() -> Result<API> {
    let mut media_engine = MediaEngine::default();
    // No media tracks are ever added; registering default codecs just
    // keeps SDP generation on the well-trodden path the examples use.
    media_engine.register_default_codecs()?;
    let registry = register_default_interceptors(Registry::new(), &mut media_engine)?;
    Ok(APIBuilder::new()
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .build())
}

fn ice_config() -> RTCConfiguration {
    RTCConfiguration {
        // TODO(turn): strict/symmetric NATs need a TURN relay to connect at
        // all; STUN alone only resolves reachable addresses. Add
        // `RTCIceServer { urls: vec![turn_url], username, credential, .. }`
        // here (or thread an `Option<TurnConfig>` through connect_as_*) once
        // TURN infra exists. Deferred for the MVP - STUN-only proves the
        // core transport loop on a LAN, which is what the demo needs.
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// Registers on_open/on_message on a data channel and returns receivers for
/// each. Must be called synchronously within (or immediately after)
/// whatever creates `dc`, before yielding control, so no open/message event
/// can fire before we're listening for it.
fn wire_data_channel(
    dc: &Arc<RTCDataChannel>,
) -> (oneshot::Receiver<()>, mpsc::UnboundedReceiver<Bytes>) {
    let (open_tx, open_rx) = oneshot::channel::<()>();
    dc.on_open(Box::new(move || {
        let _ = open_tx.send(());
        Box::pin(async {})
    }));

    let (msg_tx, msg_rx) = mpsc::unbounded_channel::<Bytes>();
    dc.on_message(Box::new(move |msg: DataChannelMessage| {
        let _ = msg_tx.send(msg.data);
        Box::pin(async {})
    }));

    (open_rx, msg_rx)
}

/// Connects to the signaling server as the WebRTC offerer: creates the data
/// channel and sends the initial offer. Room-code pairing means it doesn't
/// matter whether the sender or receiver connects first.
pub async fn connect_as_sender(signaling_url: &str, room_code: &str) -> Result<DataChannelConn> {
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        connect_as_sender_inner(signaling_url, room_code),
    )
    .await
    .context("timed out connecting to peer")?
}

async fn connect_as_sender_inner(signaling_url: &str, room_code: &str) -> Result<DataChannelConn> {
    let api = build_api()?;
    let pc = Arc::new(api.new_peer_connection(ice_config()).await?);
    let (signaling, mut inbound_rx) = SignalingClient::connect(signaling_url, room_code).await?;

    let dc = pc.create_data_channel("data", None).await?;
    let (open_rx, msg_rx) = wire_data_channel(&dc);

    // Non-trickle ICE: wait for gathering to finish before sending the
    // offer, so it already carries every local candidate.
    let mut gather_complete = pc.gathering_complete_promise().await;
    let offer = pc.create_offer(None).await?;
    pc.set_local_description(offer).await?;
    let _ = gather_complete.recv().await;
    let local_desc = pc
        .local_description()
        .await
        .context("no local description after ICE gathering completed")?;
    signaling.send(SignalMsg::Offer { data: local_desc })?;

    let answer = recv_sdp(&mut inbound_rx)
        .await
        .context("signaling closed before an answer arrived")?;
    pc.set_remote_description(answer).await?;

    open_rx
        .await
        .context("data channel closed before it finished opening")?;

    Ok(DataChannelConn {
        dc,
        incoming: AsyncMutex::new(msg_rx),
        _pc: pc,
    })
}

/// Connects to the signaling server as the WebRTC answerer: waits for the
/// peer's offer, replies with an answer, and receives the data channel the
/// peer created.
pub async fn connect_as_receiver(
    signaling_url: &str,
    room_code: &str,
) -> Result<DataChannelConn> {
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        connect_as_receiver_inner(signaling_url, room_code),
    )
    .await
    .context("timed out connecting to peer")?
}

async fn connect_as_receiver_inner(
    signaling_url: &str,
    room_code: &str,
) -> Result<DataChannelConn> {
    let api = build_api()?;
    let pc = Arc::new(api.new_peer_connection(ice_config()).await?);
    let (signaling, mut inbound_rx) = SignalingClient::connect(signaling_url, room_code).await?;

    // The remote data channel arrives asynchronously via this callback. Its
    // on_open/on_message handlers MUST be registered inside the callback,
    // synchronously, before it returns - the underlying transport awaits
    // this callback's future to completion and only then flips the channel
    // to "open" and starts delivering messages. Wiring up handlers any
    // later than that risks missing the open event or dropping early
    // messages (see webrtc-rs sctp_transport.rs: on_data_channel_handler is
    // awaited, then handle_open() runs).
    type DcReady = (
        Arc<RTCDataChannel>,
        oneshot::Receiver<()>,
        mpsc::UnboundedReceiver<Bytes>,
    );
    let (dc_ready_tx, dc_ready_rx) = oneshot::channel::<DcReady>();
    let dc_ready_tx = StdMutex::new(Some(dc_ready_tx));
    pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
        let (open_rx, msg_rx) = wire_data_channel(&dc);
        if let Some(tx) = dc_ready_tx.lock().unwrap().take() {
            let _ = tx.send((dc, open_rx, msg_rx));
        }
        Box::pin(async {})
    }));

    let offer = recv_sdp(&mut inbound_rx)
        .await
        .context("signaling closed before an offer arrived")?;
    pc.set_remote_description(offer).await?;

    let mut gather_complete = pc.gathering_complete_promise().await;
    let answer = pc.create_answer(None).await?;
    pc.set_local_description(answer).await?;
    let _ = gather_complete.recv().await;
    let local_desc = pc
        .local_description()
        .await
        .context("no local description after ICE gathering completed")?;
    signaling.send(SignalMsg::Answer { data: local_desc })?;

    let (dc, open_rx, msg_rx) = dc_ready_rx
        .await
        .context("peer never opened a data channel")?;
    open_rx
        .await
        .context("data channel closed before it finished opening")?;

    Ok(DataChannelConn {
        dc,
        incoming: AsyncMutex::new(msg_rx),
        _pc: pc,
    })
}

/// Sends `data` over the channel, split into chunks of at most
/// [`CHUNK_SIZE`] bytes, preceded by an 8-byte big-endian length prefix
/// (its own message) so the receiver knows the total size up front.
/// `on_progress(bytes_sent, total)` fires after each chunk.
pub async fn send_payload(
    conn: &DataChannelConn,
    data: &[u8],
    mut on_progress: impl FnMut(usize, usize),
) -> Result<()> {
    let total = data.len();
    conn.dc
        .send(&Bytes::copy_from_slice(&(total as u64).to_be_bytes()))
        .await
        .context("failed to send length prefix")?;

    let mut sent = 0usize;
    for chunk in data.chunks(CHUNK_SIZE) {
        conn.dc
            .send(&Bytes::copy_from_slice(chunk))
            .await
            .context("failed to send chunk")?;
        sent += chunk.len();
        on_progress(sent, total);
    }
    Ok(())
}

/// Receives a full payload: the first message on the channel is always the
/// 8-byte big-endian total length, followed by chunks until that many bytes
/// have arrived. `on_progress(bytes_received, total)` fires after each
/// chunk.
pub async fn receive_payload(
    conn: &DataChannelConn,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<Vec<u8>> {
    let mut incoming = conn.incoming.lock().await;

    let prefix = incoming
        .recv()
        .await
        .context("channel closed before the length prefix arrived")?;
    anyhow::ensure!(
        prefix.len() == 8,
        "expected an 8-byte length prefix, got {} bytes",
        prefix.len()
    );
    let total = u64::from_be_bytes(prefix[..8].try_into().unwrap()) as usize;

    let mut buf = Vec::with_capacity(total);
    while buf.len() < total {
        let chunk = incoming
            .recv()
            .await
            .context("channel closed before the full payload arrived")?;
        buf.extend_from_slice(&chunk);
        on_progress(buf.len(), total);
    }
    Ok(buf)
}
