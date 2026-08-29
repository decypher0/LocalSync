//! LAN-auto-discovery glue for the "no manual signaling URL" flow: an
//! embedded relay server, LAN IP detection, and a compact room-code
//! encoding that packs both into one string a user can read aloud or
//! paste, instead of a human running a separate signaling-server process
//! and typing its address.
//!
//! The relay here replicates `apps/signaling-server/index.js` byte-for-byte
//! (room-code-keyed pairing of the first two connecting clients, blind JSON
//! relay between them, a queue for messages sent before the second peer
//! joins, notify-and-cleanup on disconnect) so `signaling.rs`'s client - and
//! therefore `connect_as_sender`/`connect_as_receiver` - works against it
//! with zero changes.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;

// ==================== LAN IP detection ====================

/// Detects this machine's LAN-facing IPv4 address by asking the OS routing
/// table which local interface it would use to reach a public address - the
/// classic no-new-dependency trick: `connect()` on a UDP socket is just a
/// routing-table lookup, it never actually sends a packet or needs real
/// internet connectivity.
pub fn detect_lan_ip() -> Result<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").context("failed to bind UDP socket")?;
    socket
        .connect("8.8.8.8:80")
        .context("failed to resolve a route to the LAN interface")?;
    match socket
        .local_addr()
        .context("failed to read local address")?
        .ip()
    {
        std::net::IpAddr::V4(ip) => Ok(ip),
        std::net::IpAddr::V6(_) => {
            anyhow::bail!("local route resolved to an IPv6 address, expected IPv4")
        }
    }
}

// ==================== room-code encode/decode ====================

const B62_ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// `IP(4) + port(2) + room id(4)` = 10 bytes = 80 bits. `62^14 > 2^80`, so 14
/// fixed-width, zero-padded base62 digits round-trip every possible value
/// with no leading-zero ambiguity (the classic pitfall of encoding raw bytes
/// as a variable-width big integer).
const ROOM_CODE_DIGITS: usize = 14;

/// Packs `addr` and a room id into a short, alphanumeric, fixed-width
/// (`ROOM_CODE_DIGITS`-character) code. `room_id`'s bytes are packed
/// verbatim (truncated/zero-padded to 4) - this isn't a security boundary,
/// just collision avoidance for the relay's room map, so a short id is
/// plenty; see [`generate_room_id`] for the id this is meant to be called
/// with.
pub fn encode_room_code(addr: SocketAddrV4, room_id: &str) -> String {
    let mut bytes = [0u8; 10];
    bytes[0..4].copy_from_slice(&addr.ip().octets());
    bytes[4..6].copy_from_slice(&addr.port().to_be_bytes());
    let id = room_id.as_bytes();
    let n = id.len().min(4);
    bytes[6..6 + n].copy_from_slice(&id[..n]);

    let mut value: u128 = 0;
    for b in bytes {
        value = (value << 8) | b as u128;
    }

    let mut digits = [0u8; ROOM_CODE_DIGITS];
    for slot in digits.iter_mut().rev() {
        *slot = B62_ALPHABET[(value % 62) as usize];
        value /= 62;
    }
    String::from_utf8(digits.to_vec()).expect("base62 alphabet is ASCII")
}

/// Inverse of [`encode_room_code`].
pub fn decode_room_code(code: &str) -> Result<(SocketAddrV4, String)> {
    anyhow::ensure!(
        code.len() == ROOM_CODE_DIGITS,
        "room code must be {ROOM_CODE_DIGITS} characters, got {}",
        code.len()
    );

    let mut value: u128 = 0;
    for c in code.bytes() {
        let digit = B62_ALPHABET
            .iter()
            .position(|&b| b == c)
            .with_context(|| format!("invalid room code character: {:?}", c as char))?;
        value = value * 62 + digit as u128;
    }

    // value < 2^80, so its big-endian bytes are 6 leading zero bytes
    // followed by exactly the 10 bytes encode_room_code packed.
    let bytes = value.to_be_bytes();
    let ip = Ipv4Addr::new(bytes[6], bytes[7], bytes[8], bytes[9]);
    let port = u16::from_be_bytes([bytes[10], bytes[11]]);
    let room_id =
        String::from_utf8(bytes[12..16].to_vec()).context("room id bytes were not valid UTF-8")?;
    Ok((SocketAddrV4::new(ip, port), room_id))
}

/// Generates a 4-character alphanumeric room id for [`encode_room_code`].
///
/// Cheap process-local randomness, not a security boundary (see module
/// doc) - just enough to keep two concurrent senders from colliding on the
/// same relay's room map. Mixes wall-clock time, pid, and a per-process
/// counter through splitmix64 rather than pulling in a `rand` dependency
/// for 4 bytes of collision-avoidance entropy.
pub fn generate_room_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ (std::process::id() as u64)
        ^ COUNTER
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(0x9E3779B97F4A7C15);

    let mut out = String::with_capacity(4);
    for _ in 0..4 {
        seed = seed.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^= z >> 31;
        out.push(B62_ALPHABET[(z % 62) as usize] as char);
    }
    out
}

// ==================== embedded relay server ====================

type Tx = mpsc::UnboundedSender<Message>;

#[derive(Default)]
struct RoomState {
    /// Inbox senders in join order (index 0 = first joiner). Forwarding
    /// looks up "the other index" once both are present - mirrors
    /// index.js's `first.peer = second; second.peer = first`.
    txs: Vec<Tx>,
    /// Messages sent by the sole connected client before a second joins -
    /// flushed to the second joiner's inbox the moment it pairs, so nothing
    /// sent right after connecting (e.g. an eager offer) is lost to the
    /// race. Mirrors index.js's `entry.queue`.
    queue: Vec<Message>,
}

type Rooms = Arc<StdMutex<HashMap<String, RoomState>>>;

/// Starts an embedded signaling relay implementing the exact protocol
/// `apps/signaling-server/index.js` does, so `connect_as_sender` /
/// `connect_as_receiver` work against it unmodified.
///
/// Binds `0.0.0.0:0` (not loopback - this must be reachable from other
/// machines on the LAN) and lets the OS pick a free port. Returns that port
/// plus a handle to the background accept loop.
///
/// ponytail: runs until the process exits, no shutdown/expiry - fine at
/// this app's scale (one relay per Send session, torn down when the app
/// quits). Add explicit shutdown if LocalSync ever needs to free the port
/// mid-run.
pub async fn host_ephemeral_relay() -> Result<(u16, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("0.0.0.0:0")
        .await
        .context("failed to bind ephemeral relay port")?;
    let port = listener
        .local_addr()
        .context("failed to read bound relay port")?
        .port();
    let rooms: Rooms = Arc::new(StdMutex::new(HashMap::new()));
    let handle = tokio::spawn(relay_accept_loop(listener, rooms));
    Ok((port, handle))
}

async fn relay_accept_loop(listener: TcpListener, rooms: Rooms) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                log::warn!("relay: accept failed: {e}");
                continue;
            }
        };
        let rooms = rooms.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_relay_connection(stream, rooms).await {
                log::debug!("relay: connection ended: {e:#}");
            }
        });
    }
}

fn close_frame(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame {
        code: CloseCode::from(code),
        reason: reason.into(),
    }))
}

async fn handle_relay_connection(stream: TcpStream, rooms: Rooms) -> Result<()> {
    // Callback runs during the handshake (it's FnOnce, called exactly
    // once); it can't return the path directly, so it writes into a shared
    // slot this function reads right after accept_hdr_async resolves.
    let room_code_slot = Arc::new(StdMutex::new(String::new()));
    let slot = room_code_slot.clone();
    let callback = move |req: &Request, resp: Response| {
        *slot.lock().unwrap() = req.uri().path().trim_start_matches('/').to_string();
        Ok(resp)
    };
    let ws_stream = tokio_tungstenite::accept_hdr_async(stream, callback)
        .await
        .context("websocket handshake failed")?;
    let room_code = room_code_slot.lock().unwrap().clone();

    let (mut write, mut read) = ws_stream.split();
    if room_code.is_empty() {
        let _ = write.send(close_frame(1008, "room code required")).await;
        return Ok(());
    }

    let (my_tx, mut my_rx) = mpsc::unbounded_channel::<Message>();
    // Locked section decides Full-vs-Joined and returns a plain value; the
    // MutexGuard (not Send) is dropped at this block's unconditional end,
    // never held across the `.await` below - needed for the outer
    // `tokio::spawn`'d future to type-check as Send.
    enum Join {
        Full,
        Joined(usize),
    }
    let join = {
        let mut rooms_g = rooms.lock().unwrap();
        let room = rooms_g.entry(room_code.clone()).or_default();
        if room.txs.len() >= 2 {
            Join::Full
        } else {
            let idx = room.txs.len();
            room.txs.push(my_tx);
            if room.txs.len() == 2 {
                for msg in std::mem::take(&mut room.queue) {
                    let _ = room.txs[1].send(msg);
                }
            }
            Join::Joined(idx)
        }
    };
    let my_index = match join {
        Join::Full => {
            let _ = write.send(close_frame(1008, "room full")).await;
            return Ok(());
        }
        Join::Joined(idx) => idx,
    };
    log::info!("relay: join room={room_code} peer_index={my_index}");

    // Writer: drain this connection's inbox (fed by whichever side is its
    // peer) onto its actual websocket.
    let writer = tokio::spawn(async move {
        while let Some(msg) = my_rx.recv().await {
            if write.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Reader: forward anything JSON-parseable to the peer once paired,
    // else queue it - "not our problem, ignore anything that isn't JSON"
    // matches index.js's try/catch-and-skip.
    while let Some(Ok(msg)) = read.next().await {
        let Message::Text(text) = &msg else { continue };
        if serde_json::from_str::<serde_json::Value>(text).is_err() {
            continue;
        }
        let mut rooms_g = rooms.lock().unwrap();
        if let Some(room) = rooms_g.get_mut(&room_code) {
            if room.txs.len() == 2 {
                let peer = 1 - my_index;
                let _ = room.txs[peer].send(msg);
            } else {
                room.queue.push(msg);
            }
        }
    }

    // Disconnect: notify the peer (if paired) and drop the whole room -
    // mirrors index.js's unconditional `rooms.delete(room)`.
    {
        let mut rooms_g = rooms.lock().unwrap();
        if let Some(room) = rooms_g.remove(&room_code) {
            if room.txs.len() == 2 {
                let peer = 1 - my_index;
                let _ = room.txs[peer].send(Message::text(r#"{"type":"peer-disconnected"}"#));
            }
        }
    }
    let _ = writer.await;
    Ok(())
}
