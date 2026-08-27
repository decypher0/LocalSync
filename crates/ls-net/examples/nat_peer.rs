//! Standalone CLI peer for exercising `ls-net` across real network
//! boundaries (e.g. containers on opposite sides of a simulated NAT). Not
//! part of the library's public contract - a thin wrapper around
//! `connect_as_sender`/`connect_as_receiver` + `send_payload`/
//! `receive_payload` that an orchestrating test can spawn as a subprocess
//! and read stdout from.
//!
//! Usage:
//!   cargo run --example nat_peer -p ls-net -- --role sender --signaling-url ws://... --room CODE [--payload-size N]
//!   cargo run --example nat_peer -p ls-net -- --role receiver --signaling-url ws://... --room CODE [--expect-sha256 HEX]
//!
//! TURN is controlled the same way it is for the library: via the
//! `LOCALSYNC_TURN_URL` / `LOCALSYNC_TURN_USERNAME` / `LOCALSYNC_TURN_CREDENTIAL`
//! environment variables that `ice_config()` already reads. There's
//! deliberately no `--turn` flag - the orchestrating test controls TURN by
//! setting or unsetting those vars in this process's environment before
//! spawning it, not by CLI argument.
//!
//! On success, prints exactly one final line to stdout:
//!   RESULT=OK PATH=Direct|Relayed|Unknown
//! and exits 0. On failure:
//!   RESULT=FAIL <reason>
//! and exits 1. The sender prints `SHA256=<hex>` before connecting (so it's
//! available even if the connection later fails); the receiver prints its
//! own `SHA256=<hex>` of whatever it actually received, after the transfer
//! completes. An orchestrating test can pass the sender's hash to the
//! receiver via `--expect-sha256` for an automatic byte-for-byte check (the
//! receiver then fails loudly on mismatch), or just diff the two printed
//! hashes itself - either way this proves "the exact same bytes arrived",
//! not just "some bytes arrived".

use std::time::Duration;

use anyhow::Context;
use ls_net::{
    connect_as_receiver, connect_as_sender, receive_payload, send_payload, ConnectionPath,
};
use sha2::{Digest, Sha256};

const DEFAULT_PAYLOAD_SIZE: usize = 262_144;

// ponytail: lib.rs's receive_payload/send_payload deliberately have no
// timeout ("large payloads just take longer" - see lib.rs doc comment).
// Verified during this round: two separate `nat_peer` OS processes on the
// same host can stall mid-transfer and never recover, even minutes later -
// same-process transfers (tests/transfer.rs, two tokio tasks) don't show
// this. Looks like a real reliability gap in webrtc-rs's SCTP retransmission
// under actual process/OS scheduling, not something pacing or TURN
// explains (reproduces with TURN unset). Out of scope to fix here; this
// timeout just keeps a stuck subprocess from hanging an orchestrating test
// forever. Upgrade: retry the transfer, or fix/replace the SCTP layer.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(60);

struct Args {
    role: String,
    signaling_url: String,
    room: String,
    payload_size: usize,
    expect_sha256: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut role = None;
    let mut signaling_url = None;
    let mut room = None;
    let mut payload_size = DEFAULT_PAYLOAD_SIZE;
    let mut expect_sha256 = None;

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{flag} requires a value"));
        match flag.as_str() {
            "--role" => role = Some(val()?),
            "--signaling-url" => signaling_url = Some(val()?),
            "--room" => room = Some(val()?),
            "--payload-size" => {
                payload_size = val()?
                    .parse()
                    .map_err(|e| format!("invalid --payload-size: {e}"))?
            }
            "--expect-sha256" => expect_sha256 = Some(val()?),
            other => return Err(format!("unknown flag: {other}")),
        }
    }

    Ok(Args {
        role: role.ok_or("missing --role")?,
        signaling_url: signaling_url.ok_or("missing --signaling-url")?,
        room: room.ok_or("missing --room")?,
        payload_size,
        expect_sha256,
    })
}

/// Deterministic pseudo-random bytes: same `n` always produces the same
/// output, so different processes (or a receiver that wants to regenerate
/// rather than hash-compare) agree without coordination. Same formula as
/// `tests/transfer.rs` uses for its payload.
fn deterministic_payload(n: usize) -> Vec<u8> {
    (0..n as u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect()
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn path_label(p: ConnectionPath) -> &'static str {
    match p {
        ConnectionPath::Direct => "Direct",
        ConnectionPath::Relayed => "Relayed",
        ConnectionPath::Unknown => "Unknown",
    }
}

async fn run(args: Args) -> anyhow::Result<ConnectionPath> {
    match args.role.as_str() {
        "sender" => {
            let payload = deterministic_payload(args.payload_size);
            // Printed before connecting so it's available even if the
            // connection itself later fails.
            println!("SHA256={}", sha256_hex(&payload));

            let conn = connect_as_sender(&args.signaling_url, &args.room).await?;
            tokio::time::timeout(TRANSFER_TIMEOUT, send_payload(&conn, &payload, |_, _| {}))
                .await
                .context("timed out sending payload")??;
            Ok(conn.connection_path().await)
        }
        "receiver" => {
            let conn = connect_as_receiver(&args.signaling_url, &args.room).await?;
            let received =
                tokio::time::timeout(TRANSFER_TIMEOUT, receive_payload(&conn, |_, _| {}))
                    .await
                    .context("timed out receiving payload")??;
            let actual = sha256_hex(&received);
            println!("SHA256={actual}");
            if let Some(expect) = &args.expect_sha256 {
                anyhow::ensure!(
                    &actual == expect,
                    "sha256 mismatch: expected {expect}, got {actual} ({} bytes received)",
                    received.len()
                );
            }
            Ok(conn.connection_path().await)
        }
        other => anyhow::bail!("unknown --role {other:?}, expected \"sender\" or \"receiver\""),
    }
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            println!("RESULT=FAIL {e}");
            std::process::exit(1);
        }
    };

    match run(args).await {
        Ok(path) => {
            println!("RESULT=OK PATH={}", path_label(path));
        }
        Err(e) => {
            println!("RESULT=FAIL {e:#}");
            std::process::exit(1);
        }
    }
}
