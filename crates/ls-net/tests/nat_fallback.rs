//! Proves TURN actually rescues the connection when a direct P2P path is
//! genuinely impossible - not just "TURN is configured and reachable"
//! (that's `turn_configured_still_prefers_direct.rs`'s job), but "direct is
//! blocked and the handshake+transfer still completes via the relay".
//!
//! The network boundary: one shared Podman network holds a coturn relay and
//! a signaling server (standing in for "public infrastructure" reachable by
//! both peers), plus two peer containers. Each peer container gets
//! `--cap-add=NET_ADMIN` and, before running `nat_peer`, switches its own
//! network namespace to default-deny (`iptables -P INPUT/OUTPUT DROP`) with
//! an allowlist of exactly loopback + the TURN IP + the signaling IP. This
//! needs no host root, only the container's own already-isolated netns -
//! `ip netns`/`iptables` on the host itself would need real root via `sudo`,
//! which isn't available here.
//!
//! Three narrower designs were tried first and rejected, each ruled out by
//! direct experiment rather than assumed:
//!   - Two independently-created Podman bridge networks, one peer on each:
//!     on this box's rootless netavark setup, containers on separate
//!     bridges could still reach each other directly at the IP layer
//!     (confirmed with a raw TCP connect test), so ICE just found a working
//!     direct path and the isolation was fiction.
//!   - One shared network with only the *other peer's specific IP* dropped
//!     (not a full allowlist): this still let each side reach the public
//!     internet, so STUN handed out a real reflexive address and ICE found a
//!     hairpin path through the shared host NAT anyway.
//!   - A full allowlist of loopback + the *entire* TURN IP (any port) +
//!     signaling IP: this closed the STUN leak, but every connection still
//!     completed as `PATH=Direct` for one peer and `PATH=Relayed` for the
//!     other. Root cause (confirmed with a negative control - same lockdown,
//!     `LOCALSYNC_TURN_*` unset - which then made the connection fail
//!     completely, `RESULT=FAIL ... deadline has elapsed`, proving there
//!     really was no other path): `connection_path()` reports each side's
//!     *own* local candidate type, and coturn's dynamic relay ports
//!     (`min-port`/`max-port`) are reachable directly by anyone who can
//!     reach the TURN server's IP at all. So one peer could address the
//!     other's relay allocation as an ordinary remote endpoint using its own
//!     everyday "host" candidate - genuinely routed through the relay, just
//!     not classified as such from *that* peer's side. Restricting each
//!     peer's allowlisted access to the TURN IP down to just the control
//!     port (3478 - `LOCALSYNC_TURN_URL`'s port) and denying the dynamic
//!     relay-port range closes that shortcut: neither peer can reach the
//!     other's relay allocation directly, so both are forced to allocate and
//!     use their own, and both correctly report `PATH=Relayed`.
//!
//! Skips (prints why, doesn't fail) if `podman` isn't on PATH, so `cargo
//! test` still works on a box without Podman - same gating as
//! `ls-containers`' own podman-backed integration test.
//!
//! Requires `target/debug/examples/nat_peer` already built:
//!   cargo build --example nat_peer -p ls-net

use std::time::Duration;

use sha2::{Digest, Sha256};

const NETWORK: &str = "localsync-nat-test";
const SUBNET: &str = "10.99.99.0/24";
const TURN_IP: &str = "10.99.99.2";
const SIGNALING_IP: &str = "10.99.99.3";
const PEER_A_IP: &str = "10.99.99.4";
const PEER_B_IP: &str = "10.99.99.5";
const SIGNALING_PORT: u16 = 9090;
const PAYLOAD_SIZE: usize = 4096; // see nat_peer.rs: larger sizes reproducibly stall cross-process

const CONTAINERS: &[&str] = &[
    "ls-nat-peer-a",
    "ls-nat-peer-b",
    "ls-nat-turn",
    "ls-nat-signaling",
];

fn podman_available() -> bool {
    std::process::Command::new("podman")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Best-effort teardown: remove every container/network this test could
/// have created, ignoring errors (nothing to remove is not a failure). Used
/// both as a defensive pre-clean (leftovers from a prior run shouldn't block
/// this one) and via `Cleanup`'s `Drop` impl, so it runs on success, on
/// assertion failure/panic, and on early return alike.
fn cleanup_all() {
    for name in CONTAINERS {
        let _ = std::process::Command::new("podman")
            .args(["rm", "-f", name])
            .output();
    }
    let _ = std::process::Command::new("podman")
        .args(["network", "rm", "-f", NETWORK])
        .output();
}

struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        cleanup_all();
    }
}

/// Runs a podman subcommand, panicking with its stderr on failure.
async fn podman(args: &[&str]) {
    let out = tokio::process::Command::new("podman")
        .args(args)
        .output()
        .await
        .unwrap_or_else(|e| panic!("failed to spawn `podman {args:?}`: {e}"));
    assert!(
        out.status.success(),
        "`podman {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Polls `podman logs <name>` until it contains `marker`, or panics after
/// `timeout`.
async fn wait_for_log(name: &str, marker: &str, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let out = tokio::process::Command::new("podman")
            .args(["logs", name])
            .output()
            .await
            .expect("failed to run podman logs");
        if String::from_utf8_lossy(&out.stdout).contains(marker)
            || String::from_utf8_lossy(&out.stderr).contains(marker)
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{name} never logged {marker:?} within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// Same formula `nat_peer`'s `deterministic_payload` uses, so the expected
/// hash can be computed up front instead of trusting either peer's own
/// printed hash.
fn expected_sha256_hex(n: usize) -> String {
    let payload: Vec<u8> = (0..n as u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();
    let mut hasher = Sha256::new();
    hasher.update(&payload);
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The last `RESULT=...` line of a peer's stdout, or a panic dumping the
/// full stdout/stderr for debugging.
fn extract_result(label: &str, stdout: &str, stderr: &str) -> String {
    stdout
        .lines()
        .rev()
        .find(|l| l.starts_with("RESULT="))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            panic!("{label}: no RESULT= line in output\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}")
        })
}

#[tokio::test]
async fn direct_path_blocked_turn_rescues_transfer() {
    if !podman_available() {
        eprintln!("skipping direct_path_blocked_turn_rescues_transfer: podman not found on PATH");
        return;
    }

    let nat_peer = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/debug/examples/nat_peer"
    );
    assert!(
        std::path::Path::new(nat_peer).exists(),
        "nat_peer example not built - run: cargo build --example nat_peer -p ls-net"
    );

    let signaling_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/signaling-server");

    cleanup_all(); // defensive: leftovers from a previous run shouldn't block this one
    let _cleanup = Cleanup;

    podman(&["network", "create", "--subnet", SUBNET, NETWORK]).await;

    podman(&[
        "run", "-d", "--name", "ls-nat-turn",
        "--network", NETWORK, "--ip", TURN_IP,
        "docker.io/coturn/coturn",
        "-n", "--log-file=stdout", "--lt-cred-mech",
        "--realm=localsync.test", "--user=localsync:localsync-turn-pw",
        "--no-cli", "--min-port=49160", "--max-port=49200",
    ])
    .await;

    podman(&[
        "run", "-d", "--name", "ls-nat-signaling",
        "--network", NETWORK, "--ip", SIGNALING_IP,
        "-v", &format!("{signaling_dir}:/app:ro"),
        "-w", "/app", "-e", &format!("PORT={SIGNALING_PORT}"),
        "docker.io/library/node:18-slim", "node", "index.js",
    ])
    .await;

    wait_for_log("ls-nat-turn", "relay", Duration::from_secs(15)).await;
    wait_for_log("ls-nat-signaling", "listening", Duration::from_secs(15)).await;

    let room = format!("nat-fallback-{}", std::process::id());
    let expect_sha = expected_sha256_hex(PAYLOAD_SIZE);
    let turn_env = [
        "-e", "LOCALSYNC_TURN_USERNAME=localsync",
        "-e", "LOCALSYNC_TURN_CREDENTIAL=localsync-turn-pw",
    ];

    // Each peer: install iptables, then default-deny both chains and
    // allowlist only loopback + the TURN server's *control port* (3478,
    // never its dynamic relay-port range - see the module doc comment for
    // why that distinction is load-bearing) + the signaling IP, before
    // running nat_peer. This blocks the other peer's container IP, the
    // public internet (so STUN can't hand out a hairpin-able reflexive
    // address), and each peer's relay allocation as a reachable shortcut for
    // the other. NET_ADMIN only grants control over this container's own
    // already-isolated netns.
    let lockdown = format!(
        "iptables -P INPUT DROP && iptables -P OUTPUT DROP && \
         iptables -A INPUT -i lo -j ACCEPT && iptables -A OUTPUT -o lo -j ACCEPT && \
         iptables -A OUTPUT -d {TURN_IP} -p udp --dport 3478 -j ACCEPT && \
         iptables -A OUTPUT -d {TURN_IP} -p tcp --dport 3478 -j ACCEPT && \
         iptables -A INPUT -s {TURN_IP} -p udp --sport 3478 -j ACCEPT && \
         iptables -A INPUT -s {TURN_IP} -p tcp --sport 3478 -j ACCEPT && \
         iptables -A OUTPUT -d {SIGNALING_IP} -j ACCEPT && iptables -A INPUT -s {SIGNALING_IP} -j ACCEPT"
    );
    let receiver_cmd = format!(
        "apt-get -qq update >/dev/null && apt-get -qq install -y iptables >/dev/null && \
         {lockdown} && \
         /nat_peer --role receiver --signaling-url ws://{SIGNALING_IP}:{SIGNALING_PORT} \
         --room {room} --expect-sha256 {expect_sha}"
    );
    let sender_cmd = format!(
        "apt-get -qq update >/dev/null && apt-get -qq install -y iptables >/dev/null && \
         {lockdown} && \
         /nat_peer --role sender --signaling-url ws://{SIGNALING_IP}:{SIGNALING_PORT} \
         --room {room} --payload-size {PAYLOAD_SIZE}"
    );

    let mut receiver = tokio::process::Command::new("podman");
    receiver
        .args(["run", "--rm", "--name", "ls-nat-peer-a"])
        .args(["--network", NETWORK, "--ip", PEER_A_IP])
        .args(["--cap-add=NET_ADMIN"])
        .args(["-v", &format!("{nat_peer}:/nat_peer:ro")])
        .args(["-e", &format!("LOCALSYNC_TURN_URL=turn:{TURN_IP}:3478")])
        .args(turn_env)
        .args(["-e", "DEBIAN_FRONTEND=noninteractive"])
        .args(["docker.io/library/ubuntu:24.04", "bash", "-c", &receiver_cmd]);

    let mut sender = tokio::process::Command::new("podman");
    sender
        .args(["run", "--rm", "--name", "ls-nat-peer-b"])
        .args(["--network", NETWORK, "--ip", PEER_B_IP])
        .args(["--cap-add=NET_ADMIN"])
        .args(["-v", &format!("{nat_peer}:/nat_peer:ro")])
        .args(["-e", &format!("LOCALSYNC_TURN_URL=turn:{TURN_IP}:3478")])
        .args(turn_env)
        .args(["-e", "DEBIAN_FRONTEND=noninteractive"])
        .args(["docker.io/library/ubuntu:24.04", "bash", "-c", &sender_cmd]);

    let run_both = async {
        tokio::join!(receiver.output(), sender.output())
    };
    let (receiver_out, sender_out) = tokio::time::timeout(Duration::from_secs(240), run_both)
        .await
        .expect("peer containers did not finish within 240s (apt-get + connect + transfer)");
    let receiver_out = receiver_out.expect("failed to run receiver peer container");
    let sender_out = sender_out.expect("failed to run sender peer container");

    let receiver_stdout = String::from_utf8_lossy(&receiver_out.stdout).into_owned();
    let receiver_stderr = String::from_utf8_lossy(&receiver_out.stderr).into_owned();
    let sender_stdout = String::from_utf8_lossy(&sender_out.stdout).into_owned();
    let sender_stderr = String::from_utf8_lossy(&sender_out.stderr).into_owned();

    println!("--- receiver (peer A) stdout ---\n{receiver_stdout}");
    println!("--- sender (peer B) stdout ---\n{sender_stdout}");

    let receiver_result = extract_result("receiver", &receiver_stdout, &receiver_stderr);
    let sender_result = extract_result("sender", &sender_stdout, &sender_stderr);

    assert_eq!(
        receiver_result, "RESULT=OK PATH=Relayed",
        "receiver should have connected via TURN with direct blocked (got {receiver_result:?})"
    );
    assert_eq!(
        sender_result, "RESULT=OK PATH=Relayed",
        "sender should have connected via TURN with direct blocked (got {sender_result:?})"
    );

    let receiver_sha = receiver_stdout
        .lines()
        .find_map(|l| l.strip_prefix("SHA256="))
        .expect("receiver printed no SHA256= line");
    let sender_sha = sender_stdout
        .lines()
        .find_map(|l| l.strip_prefix("SHA256="))
        .expect("sender printed no SHA256= line");
    assert_eq!(receiver_sha, sender_sha, "sender/receiver hash mismatch");
    assert_eq!(
        receiver_sha, expect_sha,
        "received bytes don't match the expected deterministic payload"
    );
}
