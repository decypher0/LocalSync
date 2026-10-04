//! Home screen: every session this device knows - sent and received - with
//! the name the person gave it and whether it's running, plus naming and
//! closing a session. Sessions are persisted from the moment they exist (see
//! `session_commands::create_project_session` and
//! `receiver_session_commands::track_received_snapshot`), so this lists what
//! is on disk and survives restarts.

use serde::Serialize;
use tauri::State;

use crate::project_session::ProjectSession;
use crate::received_session::ReceivedSession;
use crate::state::AppState;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SessionListItem {
    pub id: String,
    /// "send" | "receive".
    pub kind: String,
    /// What the person named it (falls back to the project's name).
    pub name: String,
    /// Received: its containers are running, asked of Podman. Sent: always
    /// false here - a send is only "running" while a transfer is live in this
    /// process, which the frontend already knows.
    pub running: bool,
    /// RFC3339.
    pub created_at: String,
    /// RFC3339: the last send (sent) or the last push received (received).
    pub updated_at: String,
}

fn send_item(s: &ProjectSession) -> SessionListItem {
    let last_send = s.devices.iter().filter_map(|d| d.marker.as_ref()).map(|m| m.sent_at.clone()).max();
    SessionListItem {
        id: s.id.clone(),
        kind: "send".into(),
        name: s.title.clone(),
        running: false,
        created_at: s.created_at.clone(),
        updated_at: last_send.unwrap_or_else(|| s.created_at.clone()),
    }
}

fn receive_item(s: &ReceivedSession, running: &std::collections::HashSet<String>) -> SessionListItem {
    SessionListItem {
        id: s.id.clone(),
        kind: "receive".into(),
        name: if s.name.trim().is_empty() { s.title.clone() } else { s.name.clone() },
        running: running.contains(&ls_containers::compose_project_name(&s.title, &s.git_commit)),
        created_at: s.created_at.clone(),
        updated_at: s.last_received_at.clone(),
    }
}

/// Every known session, newest activity first. In-memory copies win over the
/// stored ones (they may be a moment newer). `async` in the attribute: one
/// blocking `podman ps` decides every received session's running state.
#[tauri::command(async)]
pub fn list_sessions(state: State<'_, AppState>) -> Result<Vec<SessionListItem>, String> {
    let running = ls_containers::running_compose_projects().unwrap_or_else(|e| {
        log::warn!("list_sessions: couldn't ask podman what is running ({e:#}); showing all as stopped");
        Default::default()
    });
    let mut items: Vec<SessionListItem> = Vec::new();
    let open_sends = state.project_sessions.lock().map_err(|e| e.to_string())?.clone();
    let open_receives = state.received_sessions.lock().map_err(|e| e.to_string())?.clone();
    for s in open_sends.values() {
        items.push(send_item(s));
    }
    for s in open_receives.values() {
        items.push(receive_item(s, &running));
    }
    for entry in crate::session_history::load()? {
        if items.iter().any(|i| i.id == entry.id) {
            continue;
        }
        if let Some(p) = &entry.project {
            items.push(send_item(p));
        } else if let Some(r) = &entry.received {
            items.push(receive_item(r, &running));
        }
    }
    items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(items)
}

/// Sets a session's display name (New Session -> Name) and persists it. For a
/// received session this is only the shown name: its `title` (the project's
/// own name, which its containers are named from) is left alone.
#[tauri::command]
pub fn rename_session(state: State<'_, AppState>, session_id: String, name: String) -> Result<(), String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Give the session a name.".to_string());
    }
    if let Some(s) = state.project_sessions.lock().map_err(|e| e.to_string())?.get_mut(&session_id) {
        s.title = name;
        return crate::session_history::save_project(s);
    }
    if let Some(s) = state.received_sessions.lock().map_err(|e| e.to_string())?.get_mut(&session_id) {
        s.name = name;
        return crate::session_history::save_received(s);
    }
    Err(format!("no open session with id {session_id}"))
}

/// "Close session": stops a received session's containers if they're
/// running, then removes the session - from memory and from disk - so it
/// leaves the Home list. Its database volume is kept (a cache keyed by the
/// dump, harmless to keep and costly to rebuild).
#[tauri::command]
pub async fn close_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let received = state.received_sessions.lock().map_err(|e| e.to_string())?.get(&session_id).cloned();
    let received = match received {
        Some(r) => Some(r),
        None => crate::session_history::find_received(&session_id)?,
    };
    if let Some(r) = received {
        // Current version and, after an update, the one before it.
        crate::receiver_session_commands::stop_session_containers(&state, &r).await?;
    }
    state.received_sessions.lock().map_err(|e| e.to_string())?.remove(&session_id);
    state.project_sessions.lock().map_err(|e| e.to_string())?.remove(&session_id);
    state.armed_updates.lock().map_err(|e| e.to_string())?.retain(|_, v| v != &session_id);
    crate::session_history::remove(&session_id)
}
