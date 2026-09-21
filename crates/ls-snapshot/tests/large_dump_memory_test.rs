//! Proves the fix for a real field failure: a real, large database dump
//! packaged through `share_snapshot_wizard` -> `create_snapshot_multi` hit
//! `reading dump file ...: out of memory`. The root cause traced to two
//! full-buffer copies of the dump's content: `std::fs::read()`-ing the whole
//! file in `commands.rs`, then `.clone()`-ing that `Vec<u8>` again inside
//! `create_snapshot_multi`. The fix: `PendingDump::source` can now be a
//! `DumpSource::FilePath`, which is opened and streamed in bounded chunks
//! (for both hashing and tar-appending) instead of ever being read into one
//! `Vec<u8>`.
//!
//! This test does NOT run the old, pre-fix code (it no longer exists in this
//! tree) — it can't literally show "the old code OOMs, the new code
//! doesn't". What it DOES show, honestly:
//!
//!   1. `DumpSource::FilePath` against real synthetic dumps of two very
//!      different sizes (100MB and 500MB) produces peak process memory
//!      (`VmHWM`, Linux's own high-water-mark RSS counter) that stays
//!      roughly flat rather than tracking the file size — i.e. genuinely
//!      *bounded*, not merely "smaller than before".
//!   2. `DumpSource::Bytes` fed the *same* 500MB dump (the shape a caller
//!      gets if it reads the file into memory itself before constructing a
//!      `PendingDump` — the exact thing `commands.rs` used to do
//!      unconditionally) uses peak memory that scales with the file size,
//!      as a sanity check that this measurement methodology actually
//!      detects the difference it claims to detect, rather than being
//!      insensitive to both cases for some unrelated reason (e.g. the OS
//!      page cache, or `VmHWM` not reflecting large allocations).
//!
//! Each case runs as its own freshly spawned child process (this same test
//! binary, re-invoked with `--exact <case>`), because `VmHWM` is a
//! monotonic high-water mark for the whole process's lifetime — measuring
//! several cases in one process would just accumulate, not isolate, their
//! peaks.
//!
//! Disk usage: each child creates its own dump file under a `tempfile`
//! `TempDir`, which is deleted (via `Drop`) before that child process exits
//! and the next one is spawned — so no more than ~500MB of extra disk is
//! ever alive at once, and nothing is left behind after the test finishes.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use ls_snapshot::{create_snapshot_multi, DumpSource, FolderSpec, PendingDump};
use tempfile::tempdir;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git").arg("-C").arg(dir).args(args).status().expect("git");
    assert!(status.success(), "git {args:?} failed");
}

fn make_git_folder(dir: &Path) -> std::path::PathBuf {
    let root = dir.join("project");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init"]);
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    git(&root, &["add", "-A"]);
    git(
        &root,
        &["-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-m", "init"],
    );
    root
}

/// Writes a synthetic, SQL-dump-shaped file of approximately `target_bytes`,
/// streamed out in small chunks rather than built up as one big in-memory
/// buffer first (so *generating* the fixture doesn't itself defeat the
/// point of the test).
fn write_synthetic_dump(path: &Path, target_bytes: u64) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let row = b"INSERT INTO `orders` (`id`,`customer`,`total`,`notes`) VALUES (1,'Acme Corp',199.99,'synthetic test row for large-dump memory test');\n";
    let mut written: u64 = 0;
    while written < target_bytes {
        f.write_all(row).unwrap();
        written += row.len() as u64;
    }
    f.flush().unwrap();
}

/// Current process's peak resident set size in KB, from Linux's own
/// `/proc/self/status` `VmHWM` ("high water mark") field — monotonically
/// non-decreasing for the process's lifetime, which is exactly the "did
/// this ever balloon" signal this test wants.
fn vm_hwm_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("reading /proc/self/status (Linux-only test)");
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            return rest.trim().trim_end_matches(" kB").trim().parse().expect("parsing VmHWM");
        }
    }
    panic!("VmHWM not found in /proc/self/status");
}

/// Builds one dump of `size_mb` via `source_kind` ("filepath" or "bytes"),
/// runs it through the real `create_snapshot_multi`, and prints
/// `VMHWM_KB=<n>` to stdout — read back by the driver test that spawns this
/// as a child process.
fn run_case(size_mb: u64, source_kind: &str) {
    let dir = tempdir().unwrap();
    let folder = make_git_folder(dir.path());
    let dump_path = dir.path().join("dump.sql");
    write_synthetic_dump(&dump_path, size_mb * 1024 * 1024);

    let source = match source_kind {
        "filepath" => DumpSource::FilePath(dump_path.clone()),
        "bytes" => DumpSource::Bytes(std::fs::read(&dump_path).unwrap()),
        other => panic!("unknown source_kind: {other}"),
    };

    let snapshot = create_snapshot_multi(
        &[FolderSpec { path: folder, parent_commit: None }],
        &[PendingDump { folder_index: 0, schema: "orders_db".to_string(), source, engine: "mysql".to_string() }],
    )
    .expect("create_snapshot_multi should succeed");

    // Sanity: the dump really did make it into the payload, not just an
    // empty/skipped entry — otherwise a low RSS reading would be
    // meaningless (it could just mean nothing was processed).
    assert_eq!(snapshot.manifest.database_dumps.len(), 1);
    assert_eq!(snapshot.manifest.database_dumps[0].schema, "orders_db");

    println!("VMHWM_KB={}", vm_hwm_kb());
}

#[test]
fn case_filepath_100mb() {
    run_case(100, "filepath");
}

#[test]
fn case_filepath_500mb() {
    run_case(500, "filepath");
}

#[test]
fn case_bytes_500mb() {
    run_case(500, "bytes");
}

/// Spawns `case_name` as an isolated child process (this same test binary,
/// filtered to run only that one test), and returns the `VMHWM_KB` it
/// printed.
fn spawn_case_and_read_vmhwm(case_name: &str) -> u64 {
    let exe = std::env::current_exe().expect("current_exe");
    let output = Command::new(exe)
        .args(["--exact", case_name, "--nocapture", "--test-threads=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .expect("spawning child test process");
    assert!(
        output.status.success(),
        "child test case {case_name} failed (status {:?})",
        output.status
    );
    let reader = BufReader::new(output.stdout.as_slice());
    for line in reader.lines() {
        let line = line.unwrap();
        if let Some(rest) = line.strip_prefix("VMHWM_KB=") {
            return rest.trim().parse().expect("parsing child VMHWM_KB");
        }
    }
    panic!("child test case {case_name} never printed VMHWM_KB (stdout: {:?})", String::from_utf8_lossy(&output.stdout));
}

/// The actual proof. Linux-only (reads `/proc/self/status`) — skips
/// gracefully elsewhere since this repo's CI/dev boxes are Linux/macOS/
/// Windows and this specific measurement technique is Linux-specific; the
/// fix itself (streaming, not reading `/proc`) is platform-independent.
#[test]
fn dump_file_memory_does_not_scale_linearly_with_dump_size() {
    if !cfg!(target_os = "linux") {
        eprintln!("skipping: /proc/self/status-based RSS measurement is Linux-only");
        return;
    }

    let filepath_100mb_kb = spawn_case_and_read_vmhwm("case_filepath_100mb");
    let filepath_500mb_kb = spawn_case_and_read_vmhwm("case_filepath_500mb");
    let bytes_500mb_kb = spawn_case_and_read_vmhwm("case_bytes_500mb");

    eprintln!(
        "VmHWM: filepath/100MB={filepath_100mb_kb}KB filepath/500MB={filepath_500mb_kb}KB bytes/500MB={bytes_500mb_kb}KB"
    );

    // --- 1. Bounded: streaming a 500MB dump via DumpSource::FilePath must
    // not cost anywhere near 500MB of peak RSS. 200MB (well under half the
    // file size, generously covering the binary's own baseline + git/tar/
    // gzip working buffers) is a comfortable, honest ceiling.
    let two_hundred_mb_kb = 200 * 1024;
    assert!(
        filepath_500mb_kb < two_hundred_mb_kb,
        "DumpSource::FilePath with a 500MB dump used {filepath_500mb_kb}KB peak RSS - \
         expected well under {two_hundred_mb_kb}KB if the dump is truly streamed, not buffered whole"
    );

    // --- 2. Genuinely bounded, not just "smaller": going from a 100MB dump
    // to a 500MB dump (5x) must not multiply peak RSS anywhere close to 5x.
    // Allow generous slack (2x) for noise/allocator behavior while still
    // being a real assertion against linear scaling.
    assert!(
        filepath_500mb_kb < filepath_100mb_kb * 2,
        "peak RSS scaled with dump size under DumpSource::FilePath: {filepath_100mb_kb}KB @ 100MB vs \
         {filepath_500mb_kb}KB @ 500MB - expected roughly flat, bounded memory"
    );

    // --- 3. Sanity check on the measurement itself: DumpSource::Bytes fed
    // the same 500MB dump (the old commands.rs shape - fs::read the whole
    // file up front) SHOULD show memory scaling with the file size, proving
    // this methodology can actually detect the difference it claims to.
    // If it didn't, cases 1-2 passing wouldn't mean much.
    assert!(
        bytes_500mb_kb > filepath_500mb_kb * 2,
        "expected DumpSource::Bytes (whole file read into memory) to use meaningfully more peak RSS \
         than DumpSource::FilePath for the same 500MB dump - bytes={bytes_500mb_kb}KB filepath={filepath_500mb_kb}KB; \
         if these are close, this test isn't actually sensitive to the thing it's supposed to prove"
    );
}
