//! Pure, network-free retention bookkeeping - the app-level fallback this
//! round's second research finding requires: Drive's native
//! `expirationTime` permission field only has real enforced behavior on
//! Google Workspace accounts (see the crate root and `drive.rs` doc
//! comments), so a personal-account upload must still get cleaned up on
//! time even though Drive itself won't do it.
//!
//! This module does **not** call [`crate::drive::delete_file`] or run on a
//! timer - it's data plus one pure decision function
//! ([`due_for_deletion`]). The integration layer wiring this crate into the
//! app is responsible for calling it when convenient (e.g. on app startup,
//! matching `ls_containers::ProvisioningLog`'s "check when convenient"
//! style rather than this app growing a background task scheduler) and
//! acting on the result.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::store;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Retention {
    /// Due the moment `downloaded` flips to `true` - no time cap of its
    /// own (the sender chose "delete once the receiver has it").
    DeleteAfterDownload,
    /// Due at a specific instant, downloaded or not. 24-hour and
    /// custom-date-time retention both collapse to this - the caller
    /// computes the concrete instant either way, so there's no separate
    /// "Custom" variant that would just duplicate this one.
    After(#[serde(with = "time::serde::rfc3339")] time::OffsetDateTime),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackedUpload {
    pub file_id: String,
    pub retention: Retention,
    pub downloaded: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub uploaded_at: time::OffsetDateTime,
}

const FILE_NAME: &str = "cloud_drop_uploads.json";

pub fn load_tracked_uploads() -> Result<Vec<TrackedUpload>> {
    load_tracked_uploads_in(&store::default_dir()?)
}

pub fn save_tracked_uploads(uploads: &[TrackedUpload]) -> Result<()> {
    save_tracked_uploads_in(&store::default_dir()?, uploads)
}

/// Adds `upload`, replacing any existing entry for the same `file_id`
/// (re-adding is an update, not a duplicate - same convention
/// `KnownPeers::remember` uses for re-seen pubkeys).
pub fn add_tracked_upload(upload: TrackedUpload) -> Result<()> {
    let dir = store::default_dir()?;
    let mut uploads = load_tracked_uploads_in(&dir)?;
    uploads.retain(|u| u.file_id != upload.file_id);
    uploads.push(upload);
    save_tracked_uploads_in(&dir, &uploads)
}

/// No-op if `file_id` isn't tracked (already removed, or never tracked).
pub fn mark_downloaded(file_id: &str) -> Result<()> {
    let dir = store::default_dir()?;
    let mut uploads = load_tracked_uploads_in(&dir)?;
    if let Some(u) = uploads.iter_mut().find(|u| u.file_id == file_id) {
        u.downloaded = true;
    }
    save_tracked_uploads_in(&dir, &uploads)
}

/// No-op if `file_id` isn't tracked. Call this after the integration
/// layer has actually deleted the file from Drive via
/// [`crate::drive::delete_file`] - this module never calls that itself.
pub fn remove_tracked_upload(file_id: &str) -> Result<()> {
    let dir = store::default_dir()?;
    let mut uploads = load_tracked_uploads_in(&dir)?;
    uploads.retain(|u| u.file_id != file_id);
    save_tracked_uploads_in(&dir, &uploads)
}

pub(crate) fn load_tracked_uploads_in(dir: &Path) -> Result<Vec<TrackedUpload>> {
    store::read_json_or_default(&dir.join(FILE_NAME))
}

pub(crate) fn save_tracked_uploads_in(dir: &Path, uploads: &[TrackedUpload]) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let owned: Vec<TrackedUpload> = uploads.to_vec();
    store::write_json_private(&dir.join(FILE_NAME), &owned)
}

/// Which tracked uploads should be deleted right now, per the retention
/// each one was given at upload time. Pure - no I/O, no clock reads other
/// than the `now` the caller passes in, so it's exhaustively unit-testable.
///
/// Semantics (from the round's DoD - retention is a hard cap independent
/// of download state, not merely "delete once downloaded, else never"):
/// - `DeleteAfterDownload` is due once `downloaded == true`. No
///   independent time cap of its own.
/// - `After(t)` is due once `now >= t`, **regardless** of `downloaded` -
///   the sender's chosen cap still applies even if the file was already
///   picked up.
pub fn due_for_deletion(uploads: &[TrackedUpload], now: time::OffsetDateTime) -> Vec<&TrackedUpload> {
    uploads
        .iter()
        .filter(|u| match u.retention {
            Retention::DeleteAfterDownload => u.downloaded,
            Retention::After(t) => now >= t,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upload(file_id: &str, retention: Retention, downloaded: bool) -> TrackedUpload {
        TrackedUpload {
            file_id: file_id.to_string(),
            retention,
            downloaded,
            uploaded_at: time::OffsetDateTime::now_utc(),
        }
    }

    // ---------- due_for_deletion: every combination from the DoD ----------

    #[test]
    fn after_not_downloaded_time_not_reached_is_not_due() {
        let now = time::OffsetDateTime::now_utc();
        let uploads = vec![upload("a", Retention::After(now + time::Duration::hours(1)), false)];
        assert!(due_for_deletion(&uploads, now).is_empty());
    }

    #[test]
    fn after_not_downloaded_time_reached_is_due() {
        let now = time::OffsetDateTime::now_utc();
        let uploads = vec![upload("a", Retention::After(now - time::Duration::hours(1)), false)];
        assert_eq!(due_for_deletion(&uploads, now).len(), 1);
    }

    #[test]
    fn delete_after_download_downloaded_is_due_regardless_of_time() {
        let now = time::OffsetDateTime::now_utc();
        let uploads = vec![upload("a", Retention::DeleteAfterDownload, true)];
        assert_eq!(due_for_deletion(&uploads, now).len(), 1);
        // Still due arbitrarily far in the past or future - no time cap of its own.
        assert_eq!(due_for_deletion(&uploads, now - time::Duration::days(365)).len(), 1);
        assert_eq!(due_for_deletion(&uploads, now + time::Duration::days(365)).len(), 1);
    }

    #[test]
    fn delete_after_download_not_downloaded_is_never_due() {
        let now = time::OffsetDateTime::now_utc();
        let uploads = vec![upload("a", Retention::DeleteAfterDownload, false)];
        assert!(due_for_deletion(&uploads, now).is_empty());
        assert!(due_for_deletion(&uploads, now + time::Duration::days(365)).is_empty());
    }

    #[test]
    fn after_downloaded_but_time_not_yet_reached_is_not_due_yet() {
        // Downloading early must not make an After(t) upload due early -
        // the cap is on time, not on download state.
        let now = time::OffsetDateTime::now_utc();
        let future = now + time::Duration::hours(1);
        let uploads = vec![upload("a", Retention::After(future), true)];
        assert!(due_for_deletion(&uploads, now).is_empty());
    }

    #[test]
    fn after_downloaded_and_time_reached_is_still_due() {
        // The retention cap still applies even though it was already
        // downloaded - this is the case the DoD calls out explicitly.
        let now = time::OffsetDateTime::now_utc();
        let past = now - time::Duration::hours(1);
        let uploads = vec![upload("a", Retention::After(past), true)];
        assert_eq!(due_for_deletion(&uploads, now).len(), 1);
    }

    #[test]
    fn mixed_batch_returns_only_the_due_ones() {
        let now = time::OffsetDateTime::now_utc();
        let uploads = vec![
            upload("not-due-after", Retention::After(now + time::Duration::hours(1)), false),
            upload("due-after", Retention::After(now - time::Duration::seconds(1)), false),
            upload("not-due-delete-after-download", Retention::DeleteAfterDownload, false),
            upload("due-delete-after-download", Retention::DeleteAfterDownload, true),
        ];
        let due: Vec<&str> = due_for_deletion(&uploads, now).iter().map(|u| u.file_id.as_str()).collect();
        assert_eq!(due, vec!["due-after", "due-delete-after-download"]);
    }

    // ---------- local JSON store ----------

    #[test]
    fn missing_file_loads_as_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_tracked_uploads_in(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn add_mark_downloaded_and_remove_round_trip_through_a_real_temp_file() {
        let dir = tempfile::tempdir().unwrap();

        let mut uploads = load_tracked_uploads_in(dir.path()).unwrap();
        uploads.push(upload("file-1", Retention::DeleteAfterDownload, false));
        save_tracked_uploads_in(dir.path(), &uploads).unwrap();

        let reloaded = load_tracked_uploads_in(dir.path()).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert!(!reloaded[0].downloaded);

        // mark_downloaded, replayed against this temp dir by hand (the
        // pub free functions target the real OS dir - exercised via the
        // same load/mutate/save shape they use internally).
        let mut uploads = reloaded;
        uploads[0].downloaded = true;
        save_tracked_uploads_in(dir.path(), &uploads).unwrap();
        assert!(load_tracked_uploads_in(dir.path()).unwrap()[0].downloaded);

        let mut uploads = load_tracked_uploads_in(dir.path()).unwrap();
        uploads.retain(|u| u.file_id != "file-1");
        save_tracked_uploads_in(dir.path(), &uploads).unwrap();
        assert!(load_tracked_uploads_in(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn re_adding_the_same_file_id_updates_not_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let mut uploads = vec![upload("file-1", Retention::DeleteAfterDownload, false)];
        save_tracked_uploads_in(dir.path(), &uploads).unwrap();

        uploads.retain(|u| u.file_id != "file-1");
        uploads.push(upload("file-1", Retention::DeleteAfterDownload, true));
        save_tracked_uploads_in(dir.path(), &uploads).unwrap();

        let reloaded = load_tracked_uploads_in(dir.path()).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert!(reloaded[0].downloaded);
    }
}
