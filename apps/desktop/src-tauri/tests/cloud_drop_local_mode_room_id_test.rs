//! Regression test for a real, Local-network-mode-specific bug found while
//! root-causing "Local network mode: sender's room code expires, receiver
//! never connects": `commands::start_cloud_drop_session` connected to its
//! own hosted relay with `send_info.room_code` instead of
//! `send_info.room_id`.
//!
//! For "remote" mode those two values are identical (`start_send_session`'s
//! own doc comment: a remote-mode `room_code` IS the bare room id), which is
//! exactly why this bug was invisible to `cloud_drop_protocol_test.rs` and
//! every other Cloud-drop test - none of them go through `start_send_session`
//! for real, they all connect two `ls_net` peers directly on a shared
//! `room_id` they already agree on. For "local" mode, `room_code` is the
//! 14-character IP+port+id-*encoded* string a human pastes
//! (`ls_net::encode_room_code`), not the 4-character `room_id` packed inside
//! it that `decode_room_code` extracts and that `connect_as_receiver` is
//! actually given. Connecting the sender's own relay session with the wrong,
//! longer string meant the sender's `connect_as_sender` call joined a
//! *different room* on the relay than any real receiver could ever reach
//! (`decode_room_code` always heads to the 4-character room_id) - it would
//! sit there until `ls_net::CONNECT_TIMEOUT` (300s) elapsed with no peer
//! ever arriving, then error out - exactly the "code expires, receiver never
//! connects" symptom, and *only* for local mode, since remote mode's two
//! values coincide.
//!
//! `start_cloud_drop_session` itself can't be called directly here - it
//! needs a real Google OAuth client + a live Drive upload, neither available
//! in this sandbox (see `cloud_drop_protocol_test.rs`'s own doc comment).
//! Its one defective line - which room label `connect_as_sender` uses - is
//! pulled out into `commands::cloud_drop_sender_room`, a small pure function
//! with no OAuth/Drive dependency of its own; this test calls that exact
//! function (not a hand-rolled stand-in for it), so a regression at the real
//! call site inside `start_cloud_drop_session` is caught here even though
//! the surrounding command can't be exercised end to end in this sandbox.

use std::time::Duration;

use localsync_desktop::commands;

/// Reproduces the bug exactly as it shipped: connecting with `room_code` (a
/// local-mode code's full 14-character encoded string) on one side and the
/// `room_id` a real receiver actually gets from `decode_room_code` on the
/// other. They must never pair - proven here with a short deadline instead
/// of burning the real 300s `CONNECT_TIMEOUT` to show the same thing.
#[tokio::test]
async fn local_mode_sender_connecting_with_room_code_never_pairs_with_a_real_receiver() {
    let send_info = commands::start_send_session("local".to_string(), None)
        .await
        .expect("start_send_session should succeed in local mode");
    assert_ne!(
        send_info.room_code, send_info.room_id,
        "sanity: local mode's room_code and room_id must differ for this test to mean anything \
         (they coincide in remote mode, which is exactly why this bug was local-mode-only)"
    );

    let decoded = commands::decode_room_code(send_info.room_code.clone(), None)
        .expect("decode_room_code should succeed for a real local-mode code");

    // The pre-fix shape of `start_cloud_drop_session`: connect_as_sender
    // with `room_code`, not `room_id` - a stand-in for the ORIGINAL buggy
    // inline expression `cloud_drop_sender_room` replaced (this one test
    // deliberately does NOT call `cloud_drop_sender_room` - it's the "what
    // if this regresses back to room_code" side of the proof; the sibling
    // test below calls the real function).
    let buggy_sender = ls_net::connect_as_sender(&send_info.signaling_url, &send_info.room_code);
    // What a real receiver actually does, unchanged: decode the pasted
    // code, then connect with the room_id it extracts.
    let real_receiver = ls_net::connect_as_receiver(&decoded.signaling_url, &decoded.room_id);

    // Neither side should complete within a short window - the relay never
    // pairs them because they're in two different rooms. A real assertion
    // that they *don't* connect, not just "one of them errors."
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(buggy_sender, real_receiver)
    })
    .await;
    assert!(
        outcome.is_err(),
        "a room-code-connected sender and a room-id-connected receiver must NOT pair on the \
         relay, but this join completed within 5s"
    );
}

/// The fixed shape, calling the REAL `commands::cloud_drop_sender_room` -
/// the exact function `start_cloud_drop_session` calls at its own
/// `connect_as_sender` call site. Pairs immediately, proving the fix is both
/// necessary (previous test) and sufficient, and that it stays fixed: if
/// `cloud_drop_sender_room` ever regresses back to returning `room_code`,
/// this test fails the same way the previous one demonstrates.
#[tokio::test]
async fn local_mode_sender_connecting_with_room_id_pairs_with_a_real_receiver() {
    let send_info = commands::start_send_session("local".to_string(), None)
        .await
        .expect("start_send_session should succeed in local mode");

    let decoded = commands::decode_room_code(send_info.room_code.clone(), None)
        .expect("decode_room_code should succeed for a real local-mode code");
    assert_eq!(decoded.room_id, send_info.room_id, "sanity: decode must recover the same room_id start_send_session generated");

    let room = commands::cloud_drop_sender_room(&send_info);
    let fixed_sender = ls_net::connect_as_sender(&send_info.signaling_url, room);
    let real_receiver = ls_net::connect_as_receiver(&decoded.signaling_url, &decoded.room_id);

    let (sender_result, receiver_result) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(fixed_sender, real_receiver)
    })
    .await
    .expect("sender and receiver should pair well within 10s when both connect with room_id");

    sender_result.expect("sender connect should succeed");
    receiver_result.expect("receiver connect should succeed");
}
