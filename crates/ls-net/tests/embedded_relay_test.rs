//! Proves the whole new "no manual URL" path, same-box: an embedded relay
//! (`host_ephemeral_relay`) + a room code (`encode_room_code`) that a real
//! `decode_room_code` call turns back into the exact values
//! `connect_as_sender`/`connect_as_receiver` need - no manual signaling URL
//! anywhere. `127.0.0.1` stands in for `detect_lan_ip()`'s result here
//! (LAN reachability isn't what's under test - the relay/encode/decode
//! logic is; `detect_lan_ip` itself only needs to run on a real machine,
//! not in this same-box test).

use std::net::{Ipv4Addr, SocketAddrV4};

use ls_net::{
    connect_as_receiver, connect_as_sender, decode_room_code, encode_room_code,
    generate_room_id, host_ephemeral_relay, receive_payload, send_payload,
};

#[tokio::test]
async fn same_box_transfer_using_only_a_room_code() {
    let (port, _relay_task) = host_ephemeral_relay()
        .await
        .expect("failed to start embedded relay");

    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let room_id = generate_room_id();
    let code = encode_room_code(addr, &room_id);

    // What a user would actually paste: one short alphanumeric string,
    // nothing else typed anywhere.
    assert!(code.chars().all(|c| c.is_ascii_alphanumeric()));

    let (decoded_addr, decoded_room_id) =
        decode_room_code(&code).expect("failed to decode room code");
    assert_eq!(decoded_addr, addr);
    assert_eq!(decoded_room_id, room_id);

    let signaling_url = format!("ws://{}:{}", decoded_addr.ip(), decoded_addr.port());

    let payload: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();

    let sender_url = signaling_url.clone();
    let sender_room = decoded_room_id.clone();
    let sender_payload = payload.clone();
    let sender = tokio::spawn(async move {
        let conn = connect_as_sender(&sender_url, &sender_room)
            .await
            .expect("sender failed to connect");
        send_payload(&conn, &sender_payload, |_, _| {})
            .await
            .expect("send_payload failed");
    });

    let receiver_url = signaling_url.clone();
    let receiver_room = decoded_room_id.clone();
    let receiver = tokio::spawn(async move {
        let conn = connect_as_receiver(&receiver_url, &receiver_room)
            .await
            .expect("receiver failed to connect");
        receive_payload(&conn, |_, _| {})
            .await
            .expect("receive_payload failed")
    });

    sender.await.expect("sender task panicked");
    let received = receiver.await.expect("receiver task panicked");

    assert_eq!(received, payload, "payload must round-trip byte-exact");
}

#[test]
fn room_code_round_trips_property_style() {
    // Hand-rolled property test: no proptest/quickcheck dependency for one
    // fuzz loop over three integers.
    let mut seed = 0x243F6A8885A308D3u64; // arbitrary fixed start (deterministic test)
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };

    for _ in 0..10_000 {
        let ip = Ipv4Addr::from(next() as u32);
        let port = next() as u16;
        let addr = SocketAddrV4::new(ip, port);

        // Room ids in real use are always 4 ASCII bytes (generate_room_id);
        // exercise exactly that width since encode_room_code's byte layout
        // is fixed-width and documented as taking a 4-byte id.
        let id_bytes: [u8; 4] = [
            B62[(next() % 62) as usize],
            B62[(next() % 62) as usize],
            B62[(next() % 62) as usize],
            B62[(next() % 62) as usize],
        ];
        let room_id = String::from_utf8(id_bytes.to_vec()).unwrap();

        let code = encode_room_code(addr, &room_id);
        assert_eq!(code.len(), 14, "room code must have a fixed width");
        assert!(code.chars().all(|c| c.is_ascii_alphanumeric()));

        let (decoded_addr, decoded_room_id) =
            decode_room_code(&code).expect("valid encoded code must decode");
        assert_eq!(decoded_addr, addr, "ip/port must round-trip exactly");
        assert_eq!(decoded_room_id, room_id, "room id must round-trip exactly");
    }
}

const B62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

#[test]
fn decode_rejects_wrong_length_and_bad_characters() {
    assert!(decode_room_code("short").is_err());
    assert!(decode_room_code(&"0".repeat(14).replace('0', "!")).is_err());
}

#[test]
fn generate_room_id_is_four_ascii_alphanumeric_chars_and_varies() {
    let a = generate_room_id();
    let b = generate_room_id();
    assert_eq!(a.len(), 4);
    assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
    // Not a strict guarantee (collisions are possible), but with the
    // splitmix64 counter mix, two calls in the same process matching would
    // indicate the generator is broken, not bad luck.
    assert_ne!(a, b);
}
