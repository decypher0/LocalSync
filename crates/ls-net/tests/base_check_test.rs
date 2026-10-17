//! A push asks the receiver which version it holds (`BaseCheck`) before it
//! sends, over the control channel, while the receiver is already waiting
//! for the payload on the data channel. Proves, same-box over the embedded
//! relay, that this works both ways round with an older peer:
//! - a receiver that answers gets the question and the payload;
//! - a sender that asks nothing (an older sender) is received unchanged;
//! - a receiver that never answers (an older receiver, which calls plain
//!   `receive_payload`) costs the sender only the timeout, then the payload
//!   still goes through.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use ls_net::{
    connect_as_receiver, connect_as_sender, generate_room_id, host_ephemeral_relay, receive_payload, receive_payload_answering,
    request_control, send_payload, ControlMessage,
};

async fn relay() -> String {
    let (port, _task) = host_ephemeral_relay().await.expect("failed to start embedded relay");
    format!("ws://{}:{port}", Ipv4Addr::LOCALHOST)
}

fn payload() -> Vec<u8> {
    (0..100_000u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect()
}

fn base_check() -> ControlMessage {
    ControlMessage::BaseCheck { project_id: "s-1".into(), sender_pubkey_hex: "ab".repeat(32) }
}

/// The receiver side of a new build: answers a BaseCheck with what it holds.
fn answer_with(held: Option<Vec<String>>) -> impl FnMut(ControlMessage) -> Option<ControlMessage> {
    move |msg| match msg {
        ControlMessage::BaseCheck { project_id, .. } if project_id == "s-1" => {
            Some(ControlMessage::BaseCheckResponse { commits: held.clone() })
        }
        _ => None,
    }
}

#[tokio::test]
async fn a_receiver_answers_the_base_check_and_then_gets_the_payload() {
    let url = relay().await;
    let room = generate_room_id();
    let data = payload();

    let (s_url, s_room, s_data) = (url.clone(), room.clone(), data.clone());
    let sender = tokio::spawn(async move {
        let conn = connect_as_sender(&s_url, &s_room).await.expect("sender connects");
        let reply = request_control(&conn, &base_check(), Duration::from_secs(10)).await.expect("request");
        send_payload(&conn, &s_data, |_, _| {}).await.expect("send_payload");
        reply
    });
    let conn = connect_as_receiver(&url, &room).await.expect("receiver connects");
    let received = receive_payload_answering(&conn, |_, _| {}, answer_with(Some(vec!["c1".into(), "c2".into()])))
        .await
        .expect("receive");

    assert_eq!(received, data);
    assert_eq!(
        sender.await.unwrap(),
        Some(ControlMessage::BaseCheckResponse { commits: Some(vec!["c1".into(), "c2".into()]) })
    );
}

#[tokio::test]
async fn a_receiver_with_no_such_project_says_so() {
    let url = relay().await;
    let room = generate_room_id();
    let (s_url, s_room) = (url.clone(), room.clone());
    let sender = tokio::spawn(async move {
        let conn = connect_as_sender(&s_url, &s_room).await.expect("sender connects");
        let reply = request_control(&conn, &base_check(), Duration::from_secs(10)).await.expect("request");
        send_payload(&conn, b"full", |_, _| {}).await.expect("send_payload");
        reply
    });
    let conn = connect_as_receiver(&url, &room).await.expect("receiver connects");
    let received = receive_payload_answering(&conn, |_, _| {}, answer_with(None)).await.expect("receive");
    assert_eq!(received, b"full");
    assert_eq!(sender.await.unwrap(), Some(ControlMessage::BaseCheckResponse { commits: None }));
}

#[tokio::test]
async fn an_older_sender_that_asks_nothing_is_received_unchanged() {
    let url = relay().await;
    let room = generate_room_id();
    let data = payload();
    let (s_url, s_room, s_data) = (url.clone(), room.clone(), data.clone());
    let sender = tokio::spawn(async move {
        let conn = connect_as_sender(&s_url, &s_room).await.expect("sender connects");
        send_payload(&conn, &s_data, |_, _| {}).await.expect("send_payload");
    });
    let conn = connect_as_receiver(&url, &room).await.expect("receiver connects");
    let received = tokio::time::timeout(Duration::from_secs(30), receive_payload_answering(&conn, |_, _| {}, answer_with(None)))
        .await
        .expect("must not wait for a question that never comes")
        .expect("receive");
    sender.await.unwrap();
    assert_eq!(received, data);
}

#[tokio::test]
async fn an_older_receiver_never_answers_so_the_sender_times_out_and_still_sends() {
    let url = relay().await;
    let room = generate_room_id();
    let data = payload();
    let (s_url, s_room, s_data) = (url.clone(), room.clone(), data.clone());
    let sender = tokio::spawn(async move {
        let conn = connect_as_sender(&s_url, &s_room).await.expect("sender connects");
        let started = Instant::now();
        let reply = request_control(&conn, &base_check(), Duration::from_secs(2)).await.expect("request");
        let waited = started.elapsed();
        send_payload(&conn, &s_data, |_, _| {}).await.expect("send_payload");
        (reply, waited)
    });
    let conn = connect_as_receiver(&url, &room).await.expect("receiver connects");
    let received = receive_payload(&conn, |_, _| {}).await.expect("receive");
    let (reply, waited) = sender.await.unwrap();
    assert_eq!(received, data);
    assert_eq!(reply, None, "no answer within the timeout");
    assert!(waited >= Duration::from_secs(2) && waited < Duration::from_secs(10), "{waited:?}");
}
