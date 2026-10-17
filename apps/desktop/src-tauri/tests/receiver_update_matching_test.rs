//! An incoming snapshot lands on the session this device already has of
//! that project - same sender key and the same `Manifest::project_id`, or the
//! same title for a snapshot from before project ids - as an update, even
//! when that session is only saved on disk (after a restart). A different
//! project, or the same one from another sender, is a session of its own.
//! And what a sender's `BaseCheck` is told (`held_base_commits`) follows the
//! same sessions.
//!
//! Drives `receiver_session_commands::track_received_snapshot` - what
//! `commands::finalize_received_snapshot` files every verified receive with -
//! directly with hand-built manifests, so no network, signing or Podman is
//! involved. One `#[test]`: it sets the process-wide `HOME`/`XDG_DATA_HOME`
//! (where saved sessions and known peers live).

use localsync_desktop::receiver_session_commands::{self, ReceivedSessionOutcome};
use localsync_desktop::state::AppState;
use tauri::Manager;

type Handle = tauri::AppHandle<tauri::test::MockRuntime>;

fn hex(key: &[u8; 32]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

fn manifest(title: &str, commit: &str, project_id: Option<&str>, key: [u8; 32]) -> ls_snapshot::Manifest {
    ls_snapshot::Manifest {
        project_name: title.to_string(),
        git_commit: commit.to_string(),
        git_parent_commit: None,
        dependency_lock_hash: String::new(),
        db_seed_hash: "h1".to_string(),
        services: vec![],
        sender_pubkey: key,
        created_at: time::OffsetDateTime::now_utc(),
        folders: vec![ls_snapshot::FolderInfo { name: title.to_string(), git_commit: commit.to_string(), git_parent_commit: None }],
        database_dumps: vec![ls_snapshot::DatabaseDumpEntry {
            folder: title.to_string(),
            schema: "appdb".to_string(),
            dump_file: format!("db-dumps/{title}/appdb.sql"),
            hash: "h1".to_string(),
            engine: "mysql".to_string(),
        }],
        project_id: project_id.map(String::from),
    }
}

fn track(app: &Handle, m: &ls_snapshot::Manifest) -> ReceivedSessionOutcome {
    receiver_session_commands::track_received_snapshot(
        &app.state::<AppState>(),
        m,
        &format!("{}@{}", m.project_name, m.git_commit),
        &hex(&m.sender_pubkey),
    )
    .expect("track_received_snapshot")
}

fn app() -> Handle {
    let app = tauri::test::mock_app();
    app.manage(AppState::default());
    app.handle().clone()
}

#[test]
fn an_update_lands_on_the_session_it_updates_and_nothing_else_does() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.path());
    std::env::set_var(if cfg!(windows) { "APPDATA" } else { "XDG_DATA_HOME" }, home.path().join("data"));

    let (alice, bob) = ([1u8; 32], [2u8; 32]);
    let mut peers = ls_security::KnownPeers::load_default().unwrap();
    peers.remember(alice, "Alice's laptop".to_string()).unwrap();

    let receiver = app();
    let ReceivedSessionOutcome::Created(shop) = track(&receiver, &manifest("shop", "c1", Some("p-shop"), alice)) else {
        panic!("the first receive of a project creates its session");
    };

    // ---- the same project, again: an update on that session ----
    assert_eq!(track(&receiver, &manifest("shop", "c2", Some("p-shop"), alice)), ReceivedSessionOutcome::Updated(shop.clone()));
    // Its title may change (a folder added, say) - the project id decides.
    assert_eq!(track(&receiver, &manifest("shop+api", "c3", Some("p-shop"), alice)), ReceivedSessionOutcome::Updated(shop.clone()));
    {
        let state = receiver.state::<AppState>();
        let sessions = state.received_sessions.lock().unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[&shop];
        assert_eq!((s.git_commit.as_str(), s.title.as_str()), ("c3", "shop+api"));
        assert_eq!(s.sender_name, "Alice's laptop", "who sent it is recorded with it");
        assert!(s.pending_update.is_none(), "it never ran, so there's nothing to apply over");
    }
    assert_eq!(
        receiver_session_commands::held_base_commits(&receiver.state::<AppState>(), &hex(&alice), "p-shop"),
        Some(vec!["c3".to_string()]),
        "a BaseCheck is answered with the last version received"
    );

    // ---- a different project from the same sender: its own session, even with the same title ----
    let ReceivedSessionOutcome::Created(blog) = track(&receiver, &manifest("shop+api", "b1", Some("p-blog"), alice)) else {
        panic!("a different project id is a different project");
    };
    assert_ne!(blog, shop);
    // ---- the same project id from someone else: not an update either ----
    assert!(
        matches!(track(&receiver, &manifest("shop+api", "c3", Some("p-shop"), bob)), ReceivedSessionOutcome::Created(_)),
        "another sender's project is never an update to Alice's"
    );
    assert!(receiver_session_commands::held_base_commits(&receiver.state::<AppState>(), &hex(&alice), "p-none").is_none());

    // ---- snapshots from before project ids: matched by sender + title ----
    let ReceivedSessionOutcome::Created(old) = track(&receiver, &manifest("legacy", "l1", None, alice)) else { panic!() };
    assert_eq!(track(&receiver, &manifest("legacy", "l2", None, alice)), ReceivedSessionOutcome::Updated(old.clone()));
    assert!(matches!(track(&receiver, &manifest("legacy-two", "l1", None, alice)), ReceivedSessionOutcome::Created(_)));
    assert_eq!(receiver.state::<AppState>().received_sessions.lock().unwrap().len(), 5, "shop, blog, Bob's, legacy, legacy-two");

    // ---- shop ran; an update lands while it's at that version ----
    {
        let state = receiver.state::<AppState>();
        let mut sessions = state.received_sessions.lock().unwrap();
        let s = sessions.get_mut(&shop).unwrap();
        s.record_run("/work".to_string(), "/work/shop-api-c3/shop-api".to_string());
        localsync_desktop::session_history::save_received(s).unwrap();
    }
    assert_eq!(track(&receiver, &manifest("shop+api", "c4", Some("p-shop"), alice)), ReceivedSessionOutcome::Updated(shop.clone()));
    let pending = receiver.state::<AppState>().received_sessions.lock().unwrap()[&shop].pending_update.clone();
    let pending = pending.expect("an update to a session that ran waits to be applied");
    assert_eq!(pending.previous.git_commit, "c3", "the version that ran is kept for Roll back");
    assert_eq!(
        receiver_session_commands::held_base_commits(&receiver.state::<AppState>(), &hex(&alice), "p-shop"),
        Some(vec!["c4".to_string()])
    );

    // ---- "restart": nothing in memory; the saved sessions are still matched ----
    let restarted = app();
    assert_eq!(
        receiver_session_commands::held_base_commits(&restarted.state::<AppState>(), &hex(&alice), "p-shop"),
        Some(vec!["c4".to_string()]),
        "a saved session answers a BaseCheck without being opened"
    );
    // Reopening it: the update's snapshot was only held in memory, so the
    // session is back at the version that ran.
    let view = receiver_session_commands::open_saved_received_session(restarted.state::<AppState>(), shop.clone()).expect("reopen");
    assert_eq!(view.git_commit, "c3");
    assert!(view.update.is_none(), "an update that can't be applied any more isn't offered");
    assert_eq!(view.sender_name, "Alice's laptop", "the stopped page knows who sent it");
    assert_eq!(view.databases.len(), 1);
    assert_eq!((view.databases[0].engine.as_str(), view.databases[0].schema.as_str()), ("mysql", "appdb"));
    assert_eq!(
        receiver_session_commands::held_base_commits(&restarted.state::<AppState>(), &hex(&alice), "p-shop"),
        Some(vec!["c3".to_string()]),
        "after going back, the version held is the one rolled back to"
    );

    // A push after the restart lands on the saved session it updates, never a new one.
    let another = app();
    assert_eq!(track(&another, &manifest("legacy", "l3", None, alice)), ReceivedSessionOutcome::Updated(old.clone()));
    assert_eq!(another.state::<AppState>().received_sessions.lock().unwrap().len(), 1, "the saved session was opened for it");
}
