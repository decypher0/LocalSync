//! Round 11 goal 4: proves a pull request cannot carry a payload back from
//! receiver to sender, structurally - not "the handler happens to ignore
//! extra fields today", but "there is no code path, in either direction,
//! where data from a pull request's JSON ends up anywhere in the resulting
//! Rust value or on the wire".
//!
//! `ControlMessage::PullRequest` (`crates/ls-net/src/lib.rs`) is a unit
//! variant - zero fields. That's a compile-time fact this file can't
//! literally execute (there is no way to write
//! `ControlMessage::PullRequest { payload: b"..." }` - it would not
//! compile), but it gives every test below its teeth: no matter what extra
//! keys an adversarial/hand-crafted JSON message carries, decoding it can
//! only ever produce a value with nowhere to put them.

use ls_net::ControlMessage;

/// A real pull request round-trips to exactly `{"type":"pull-request"}` -
/// nothing else could ever be serialized for this variant, since it has no
/// fields to serialize.
#[test]
fn a_real_pull_request_serializes_to_exactly_one_thing() {
    let json = serde_json::to_string(&ControlMessage::PullRequest).unwrap();
    assert_eq!(json, r#"{"type":"pull-request"}"#);
}

/// Even hand-crafted, adversarial JSON claiming to be a pull request but
/// carrying extra keys (a fake payload, a target path, arbitrary bytes)
/// decodes to the same payload-less unit variant - the extra data has
/// nowhere to go and is simply dropped, not smuggled through as some
/// dynamic/untyped field. Whatever the sender-side handler does with this
/// value (see `commands::listen_for_pull_requests` in the desktop app), it
/// can only ever be reacting to "someone asked for an update", never
/// reading attacker-supplied content - the type doesn't have a slot for it.
#[test]
fn adversarial_extra_fields_on_a_pull_request_are_inert() {
    let adversarial_payloads = [
        r#"{"type":"pull-request","payload":"cGF5bG9hZCBieXRlcw=="}"#,
        r#"{"type":"pull-request","file_contents":[1,2,3],"target_path":"/etc/passwd"}"#,
        r#"{"type":"pull-request","project_path":"../../../etc/shadow"}"#,
        r#"{"type":"pull-request","data":{"nested":{"escape":"attempt"}}}"#,
        r#"{"type":"pull-request","__proto__":{"polluted":true}}"#,
    ];

    for raw in adversarial_payloads {
        let decoded: ControlMessage = serde_json::from_str(raw)
            .unwrap_or_else(|e| panic!("expected {raw:?} to still decode (extra fields ignored): {e}"));
        assert_eq!(
            decoded,
            ControlMessage::PullRequest,
            "adversarial message {raw:?} must decode to the same payload-less PullRequest, \
             not something carrying its extra data"
        );
    }
}

/// A message that isn't even valid JSON, or whose "type" tag doesn't match
/// any real variant, is a hard decode error - never partially accepted,
/// never silently coerced into *some* variant with attacker data attached.
/// `recv_control` (lib.rs) surfaces this as an `Err`, which the desktop
/// app's control-channel listener loops treat as "channel ended" and stop -
/// never as a message to act on.
#[test]
fn garbage_or_unrecognized_messages_are_hard_errors_not_silently_accepted() {
    let garbage = [
        "not json at all",
        r#"{"type":"send-me-your-source-code"}"#,
        r#"{"payload":"no type tag at all"}"#,
        "",
    ];
    for raw in garbage {
        assert!(
            serde_json::from_str::<ControlMessage>(raw).is_err(),
            "expected {raw:?} to be rejected outright, not accepted as some ControlMessage"
        );
    }
}

/// `PullResponse`/`IncomingUpdate` (the other two variants) are the only
/// other things this channel can carry, and both flow sender -> receiver
/// only by construction in this codebase (see `commands.rs`: nothing ever
/// calls `send_control` with either variant from receiver-side code). This
/// isn't itself provable by a decode test (the type permits constructing
/// them anywhere, same as any Rust value) - documented here as what to grep
/// for if that ever changes: every `send_control(&..., &ControlMessage::` in
/// `apps/desktop/src-tauri/src/commands.rs` should be sender-side.
#[test]
fn only_eight_control_message_variants_exist_and_none_carry_arbitrary_bytes() {
    // Exhaustive match - if a new variant is ever added, this fails to
    // compile until it's handled here too, forcing a conscious decision
    // about whether it can carry a payload. Round 23 added the three
    // Cloud-drop variants: CloudAccessRequest carries only an email string
    // (an identity announcement, same class as PullRequest carrying
    // nothing at all) - never a file path, never bytes, never anything a
    // receiver could use to push content back to the sender. Round 37
    // added ConnectionRequest/ConnectionResponse for LAN discovery:
    // ConnectionRequest carries only a display name (same identity-
    // announcement class again), ConnectionResponse only a bool - neither
    // can carry a file path or bytes either.
    let all = [
        ControlMessage::PullRequest,
        ControlMessage::PullResponse { accepted: true },
        ControlMessage::IncomingUpdate,
        ControlMessage::CloudAccessRequest { google_email: "someone@example.com".to_string() },
        ControlMessage::CloudAccessResponse { accepted: true, drive_file_id: Some("abc123".to_string()) },
        ControlMessage::CloudDownloadConfirmed,
        ControlMessage::ConnectionRequest { sender_name: "Alice's Laptop".to_string() },
        ControlMessage::ConnectionResponse { accepted: true },
    ];
    for msg in all {
        match msg {
            ControlMessage::PullRequest => {}
            ControlMessage::PullResponse { accepted: _ } => {}
            ControlMessage::IncomingUpdate => {}
            ControlMessage::CloudAccessRequest { google_email: _ } => {}
            ControlMessage::CloudAccessResponse { accepted: _, drive_file_id: _ } => {}
            ControlMessage::CloudDownloadConfirmed => {}
            ControlMessage::ConnectionRequest { sender_name: _ } => {}
            ControlMessage::ConnectionResponse { accepted: _ } => {}
        }
    }
}

/// Mirrors `adversarial_extra_fields_on_a_pull_request_are_inert` above, for
/// the new identity-announcement variant specifically: a hand-crafted
/// message claiming extra fields (a fake file path, embedded bytes) still
/// decodes to a value with nowhere to put them - `google_email` is the only
/// field that exists, so that's the only thing that can ever be read back.
#[test]
fn adversarial_extra_fields_on_a_cloud_access_request_are_inert() {
    let raw = r#"{"type":"cloud-access-request","google_email":"real@example.com","payload":"evil bytes","file_path":"/etc/passwd"}"#;
    let decoded: ControlMessage = serde_json::from_str(raw).expect("expected this to still decode");
    assert_eq!(
        decoded,
        ControlMessage::CloudAccessRequest { google_email: "real@example.com".to_string() },
        "adversarial extra fields must be dropped, not smuggled through"
    );
}

/// Round 37: same shape again for `ConnectionRequest` (the LAN-discovery
/// identity announcement, sent before `share_snapshot_wizard` sends
/// anything - see `ConnectionRequest`'s own doc comment) - `sender_name` is
/// the only field that exists, so that's the only thing that can ever be
/// read back, no matter what else a hand-crafted message claims to carry.
#[test]
fn adversarial_extra_fields_on_a_connection_request_are_inert() {
    let raw = r#"{"type":"connection-request","sender_name":"Alice","payload":"evil bytes","file_path":"/etc/passwd"}"#;
    let decoded: ControlMessage = serde_json::from_str(raw).expect("expected this to still decode");
    assert_eq!(
        decoded,
        ControlMessage::ConnectionRequest { sender_name: "Alice".to_string() },
        "adversarial extra fields must be dropped, not smuggled through"
    );
}
