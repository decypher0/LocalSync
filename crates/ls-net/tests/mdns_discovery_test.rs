//! Round 37: proves announce + browse actually find each other, same-box -
//! two independent `ServiceDaemon`s (matching how the real app runs one for
//! the discoverable/receiving side and one for the browsing/sending side)
//! communicating over real multicast sockets on this machine, not a mock.
//!
//! `127.0.0.1` stands in for `detect_lan_ip()`'s result here, same
//! reasoning as `embedded_relay_test.rs`'s own comment: the *address a
//! device announces* doesn't have to be real/routable for *discovery
//! itself* to be proven working - actually connecting to a discovered
//! device is a separate concern (already covered by the existing
//! `connect_as_sender`/`connect_as_receiver` tests), not what this test is
//! about.
//!
//! Whether real multicast actually traverses a genuine home/office router
//! or switch the way it does between two processes on one machine is
//! exactly the kind of cross-machine behavior this round's own hard budget
//! rule defers to the developer's real hardware - see
//! docs/round5-manual-test-checklist.md's round 37 addendum.

use std::net::Ipv4Addr;
use std::time::Duration;

#[tokio::test]
async fn announced_peer_is_discovered_via_mdns_on_the_same_box() {
    let room_id = ls_net::generate_room_id();
    let nickname = format!("Test Device {room_id}");
    let port = 54321;

    let (announce_daemon, fullname) = ls_net::announce(&nickname, Ipv4Addr::LOCALHOST, port, &room_id)
        .expect("announce should succeed");

    let (browse_daemon, receiver) = ls_net::start_browsing().expect("start_browsing should succeed");

    // Generous timeout: mDNS resolution involves a real multicast
    // query/response round trip, not an in-memory call - a few seconds is
    // normal, not a sign of trouble.
    let found = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let event = receiver.recv_async().await.expect("browse channel closed unexpectedly");
            if let ls_net::ServiceEvent::ServiceResolved(resolved) = event {
                if let Some(peer) = ls_net::resolved_peer(&resolved) {
                    // A real LAN can have other LocalSync instances (or, in
                    // CI/shared runners, leftover ones from a previous run)
                    // announcing at the same time - matching on this test's
                    // own randomly-generated room id is what makes this
                    // assertion about *our* announcement specifically, not
                    // just "something showed up".
                    if peer.room_id == room_id {
                        return peer;
                    }
                }
            }
        }
    })
    .await
    .expect("should discover the announced peer via mDNS within the timeout");

    assert_eq!(found.nickname, nickname);
    assert_eq!(found.port, port);
    assert_eq!(found.host, Ipv4Addr::LOCALHOST);
    assert_eq!(found.room_id, room_id);

    ls_net::stop_browsing(&browse_daemon);
    ls_net::stop_announcing(&announce_daemon, &fullname);
}
