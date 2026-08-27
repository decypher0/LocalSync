//! Round 2 found that `nat_peer` (two separate OS processes, not
//! same-process tokio tasks like `transfer.rs`) reproducibly stalled
//! forever partway through transfers above ~4KB, and worked around it with
//! a tiny payload rather than fixing it - see the `ponytail:` comment that
//! used to sit on `nat_peer.rs`'s `TRANSFER_TIMEOUT` for what that
//! investigation found. This test is the fix's proof: it drives that exact
//! two-process shape (the only shape that ever showed the bug -
//! `transfer.rs`'s same-process tasks never did) at a real, measured
//! payload size, [`RUNS`] times in a row, asserting success and
//! byte-for-byte correctness on every single run. A single pass proves
//! nothing here; the original bug passed plenty of individual runs too.
//!
//! Payload size: `sample-project/`, bundled via `create_snapshot` +
//! `save_to_file` (what `crates/ls-snapshot/examples/make_snapshot.rs`
//! does - same git-init-a-copy requirement documented in
//! `apps/desktop/src-tauri/tests/preload_test.rs`), measured 18,478 bytes.
//! [`PAYLOAD_SIZE`] below is deliberately larger - real snapshots (source
//! diff + manifest + signature) range from tens of KB to low MB, and this
//! stays comfortably inside that range with margin rather than exactly
//! matching one small sample project.
//!
//! Requires the `nat_peer` example already built:
//!   cargo build --example nat_peer -p ls-net
//! (examples don't get a `CARGO_BIN_EXE_*` path the way `[[bin]]` targets
//! do, so this locates the binary in the workspace target dir directly and
//! fails with a clear message if it's missing rather than trying to build
//! it itself mid-test). Same signaling-server-override pattern as
//! `transfer.rs`: set `LS_NET_TEST_SIGNALING_URL` if `node` isn't on the
//! machine running the test.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::process::Command;

/// Comfortably inside "tens of KB to low MB" (the real range cited for
/// snapshots) with margin over the 18,478-byte measured sample-project
/// snapshot - see the module doc comment.
const PAYLOAD_SIZE: usize = 3_000_000;
const RUNS: usize = 10;
const PER_PROCESS_TIMEOUT: Duration = Duration::from_secs(90);

/// Same formula `nat_peer.rs`'s `deterministic_payload` and `transfer.rs`'s
/// payload use, so this test can compute the expected hash itself instead
/// of trusting the sender process's self-reported one for anything beyond
/// a cross-check.
fn deterministic_payload_sha256(n: usize) -> String {
    let bytes: Vec<u8> = (0..n as u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn nat_peer_bin() -> PathBuf {
    // crates/ls-net -> workspace root is two levels up.
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("failed to resolve workspace root from CARGO_MANIFEST_DIR");
    for profile in ["debug", "release"] {
        for name in ["nat_peer", "nat_peer.exe"] {
            let candidate = workspace_root.join("target").join(profile).join("examples").join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    panic!(
        "nat_peer example binary not found under {}/target/{{debug,release}}/examples - \
         run `cargo build --example nat_peer -p ls-net` first",
        workspace_root.display()
    );
}

async fn signaling_url() -> (String, Option<tokio::process::Child>) {
    if let Ok(url) = std::env::var("LS_NET_TEST_SIGNALING_URL") {
        return (url, None);
    }

    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/signaling-server/index.js"
    );
    let port = 8125; // distinct from transfer.rs (8123) and the TURN test (8124)
    let child = Command::new("node")
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

#[tokio::test]
async fn nat_peer_two_process_transfer_is_reliable_at_realistic_snapshot_size() {
    let bin = nat_peer_bin();
    let expected_sha = deterministic_payload_sha256(PAYLOAD_SIZE);
    let (url, _server) = signaling_url().await;

    for run in 0..RUNS {
        let room = format!("stress-{}-{run}", std::process::id());

        let receiver = Command::new(&bin)
            .args([
                "--role",
                "receiver",
                "--signaling-url",
                &url,
                "--room",
                &room,
                "--expect-sha256",
                &expected_sha,
            ])
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn receiver nat_peer process");

        // Same room-code-pairing tolerance as connect_as_sender/receiver
        // doc comments note - order doesn't matter to the protocol, but a
        // moment's head start avoids a pointless early room-not-found retry.
        tokio::time::sleep(Duration::from_millis(300)).await;

        let sender_out = tokio::time::timeout(
            PER_PROCESS_TIMEOUT,
            Command::new(&bin)
                .args([
                    "--role",
                    "sender",
                    "--signaling-url",
                    &url,
                    "--room",
                    &room,
                    "--payload-size",
                    &PAYLOAD_SIZE.to_string(),
                ])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap_or_else(|_| panic!("run {run}: sender process timed out after {PER_PROCESS_TIMEOUT:?}"))
        .expect("failed to run sender nat_peer process");

        let receiver_out = tokio::time::timeout(PER_PROCESS_TIMEOUT, receiver.wait_with_output())
            .await
            .unwrap_or_else(|_| panic!("run {run}: receiver process timed out after {PER_PROCESS_TIMEOUT:?}"))
            .expect("failed to wait on receiver nat_peer process");

        let sender_stdout = String::from_utf8_lossy(&sender_out.stdout);
        let receiver_stdout = String::from_utf8_lossy(&receiver_out.stdout);

        assert!(
            sender_out.status.success(),
            "run {run}: sender exited non-zero.\nstdout:\n{sender_stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&sender_out.stderr)
        );
        assert!(
            receiver_out.status.success(),
            "run {run}: receiver exited non-zero.\nstdout:\n{receiver_stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&receiver_out.stderr)
        );
        assert!(
            sender_stdout.contains("RESULT=OK"),
            "run {run}: sender did not report RESULT=OK:\n{sender_stdout}"
        );
        assert!(
            receiver_stdout.contains("RESULT=OK"),
            "run {run}: receiver did not report RESULT=OK:\n{receiver_stdout}"
        );

        let sender_sha = sender_stdout
            .lines()
            .find_map(|l| l.strip_prefix("SHA256="))
            .unwrap_or_else(|| panic!("run {run}: sender printed no SHA256= line:\n{sender_stdout}"));
        let receiver_sha = receiver_stdout
            .lines()
            .find_map(|l| l.strip_prefix("SHA256="))
            .unwrap_or_else(|| panic!("run {run}: receiver printed no SHA256= line:\n{receiver_stdout}"));

        // Belt-and-suspenders: the receiver's own --expect-sha256 check
        // already makes a mismatch fail loudly (RESULT=FAIL, asserted
        // above), but cross-check independently against our own formula
        // too - proves byte-for-byte correctness, not just "the two
        // processes agreed with each other".
        assert_eq!(sender_sha, expected_sha, "run {run}: sender's own payload hash doesn't match the expected deterministic payload");
        assert_eq!(receiver_sha, expected_sha, "run {run}: receiver's received bytes don't hash to the expected payload");
    }
}
