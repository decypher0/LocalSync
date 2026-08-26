//! Proves the actual contract: two `DataChannelConn`s - one sender, one
//! receiver - find each other through a real signaling server and move
//! bytes over a real WebRTC data channel, byte-for-byte.
//!
//! By default this spawns `apps/signaling-server/index.js` itself via
//! `node`. If `node` isn't reachable from wherever the test runs (e.g. this
//! test built inside WSL while Node only exists on the Windows host), point
//! it at an already-running server instead:
//!
//!   LS_NET_TEST_SIGNALING_URL=ws://<host-reachable-from-here>:<port> cargo test -p ls-net

use std::time::Duration;

use ls_net::{connect_as_receiver, connect_as_sender, receive_payload, send_payload};

async fn signaling_url() -> (String, Option<tokio::process::Child>) {
    if let Ok(url) = std::env::var("LS_NET_TEST_SIGNALING_URL") {
        return (url, None);
    }

    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/signaling-server/index.js"
    );
    let port = 8123;
    let child = tokio::process::Command::new("node")
        .arg(script)
        .env("PORT", port.to_string())
        .kill_on_drop(true)
        .spawn()
        .expect(
            "failed to spawn `node` for the signaling server - either install Node on this \
             machine or point LS_NET_TEST_SIGNALING_URL at an already-running instance",
        );
    // Give it a moment to start listening before we dial in.
    tokio::time::sleep(Duration::from_millis(300)).await;
    (format!("ws://127.0.0.1:{port}"), Some(child))
}

#[tokio::test]
async fn end_to_end_byte_transfer() {
    let (url, _server) = signaling_url().await;
    let room = format!("test-{}", std::process::id());

    // ~300KB of non-repeating bytes so any chunk-boundary or ordering bug
    // would show up as a mismatch rather than being masked by repetition.
    let payload: Vec<u8> = (0..300_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();

    let sender_url = url.clone();
    let sender_room = room.clone();
    let sender_payload = payload.clone();
    let sender = tokio::spawn(async move {
        let conn = connect_as_sender(&sender_url, &sender_room)
            .await
            .expect("sender failed to connect");
        let mut last = 0usize;
        send_payload(&conn, &sender_payload, |sent, total| {
            assert!(sent > last || sent == total);
            assert!(sent <= total);
            last = sent;
        })
        .await
        .expect("send_payload failed");
        assert_eq!(last, sender_payload.len());
    });

    let receiver_url = url.clone();
    let receiver_room = room.clone();
    let receiver = tokio::spawn(async move {
        let conn = connect_as_receiver(&receiver_url, &receiver_room)
            .await
            .expect("receiver failed to connect");
        let mut last = 0usize;
        let received = receive_payload(&conn, |recv, _total| {
            assert!(recv >= last);
            last = recv;
        })
        .await
        .expect("receive_payload failed");
        received
    });

    sender.await.expect("sender task panicked");
    let received = receiver.await.expect("receiver task panicked");

    assert_eq!(received.len(), payload.len());
    assert_eq!(received, payload);
}
