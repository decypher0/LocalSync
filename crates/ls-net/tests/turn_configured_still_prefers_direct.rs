//! Proves that adding TURN support didn't change same-LAN behavior: with a
//! real TURN server reachable and `LOCALSYNC_TURN_*` all set, two peers on
//! the same network still connect directly rather than through the relay.
//! TURN being configured and reachable doesn't mean it's used when a direct
//! path works - that's the fallback-only contract this round adds, and
//! `DataChannelConn::connection_path()` (backed by WebRTC stats, not a
//! guess) is what makes it checkable.
//!
//! Requires a TURN server at the address in `LOCALSYNC_TURN_URL` below
//! (default matches `scripts/start-turn.sh`'s coturn container) actually
//! running and reachable, or every connect will simply fail the same way it
//! would with an unreachable STUN server - this test doesn't spin one up
//! itself. Same signaling-server-override pattern as
//! `tests/transfer.rs`: set `LS_NET_TEST_SIGNALING_URL` if `node` isn't on
//! the machine running the test.

use std::time::Duration;

use ls_net::{connect_as_receiver, connect_as_sender, ConnectionPath};

async fn signaling_url() -> (String, Option<tokio::process::Child>) {
    if let Ok(url) = std::env::var("LS_NET_TEST_SIGNALING_URL") {
        return (url, None);
    }

    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/signaling-server/index.js"
    );
    let port = 8124; // distinct from transfer.rs's 8123 so both tests can run concurrently
    let child = tokio::process::Command::new("node")
        .arg(script)
        .env("PORT", port.to_string())
        .kill_on_drop(true)
        .spawn()
        .expect(
            "failed to spawn `node` for the signaling server - either install Node on this \
             machine or point LS_NET_TEST_SIGNALING_URL at an already-running instance",
        );
    tokio::time::sleep(Duration::from_millis(300)).await;
    (format!("ws://127.0.0.1:{port}"), Some(child))
}

fn set_turn_env_if_unset() {
    // Allow the caller to point at a different TURN deployment; otherwise
    // default to scripts/start-turn.sh's coturn container.
    for (key, default) in [
        ("LOCALSYNC_TURN_URL", "turn:localhost:3478"),
        ("LOCALSYNC_TURN_USERNAME", "localsync"),
        ("LOCALSYNC_TURN_CREDENTIAL", "localsync-turn-pw"),
    ] {
        if std::env::var(key).is_err() {
            std::env::set_var(key, default);
        }
    }
}

#[tokio::test]
async fn turn_configured_still_prefers_direct() {
    set_turn_env_if_unset();
    let (url, _server) = signaling_url().await;
    let room = format!("turn-test-{}", std::process::id());

    let sender_url = url.clone();
    let sender_room = room.clone();
    let sender = tokio::spawn(async move {
        let conn = connect_as_sender(&sender_url, &sender_room)
            .await
            .expect("sender failed to connect");
        conn.connection_path().await
    });

    let receiver_url = url.clone();
    let receiver_room = room.clone();
    let receiver = tokio::spawn(async move {
        let conn = connect_as_receiver(&receiver_url, &receiver_room)
            .await
            .expect("receiver failed to connect");
        conn.connection_path().await
    });

    let sender_path = sender.await.expect("sender task panicked");
    let receiver_path = receiver.await.expect("receiver task panicked");

    assert_eq!(
        sender_path,
        ConnectionPath::Direct,
        "sender should stay on the direct path when TURN is merely configured, not required"
    );
    assert_eq!(
        receiver_path,
        ConnectionPath::Direct,
        "receiver should stay on the direct path when TURN is merely configured, not required"
    );
}
