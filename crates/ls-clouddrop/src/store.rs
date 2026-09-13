//! Local persistence for the linked Google account's OAuth tokens.
//!
//! **Honest limitation**: this is a plain JSON file on disk (`0600` on
//! Unix, matching `ls-snapshot`'s `identity.key` precedent), not an
//! OS-keychain-backed secret store. Anything with filesystem access as this
//! OS user can read the refresh token. That's a real trade-off, not
//! something to paper over - an OS keychain (Windows Credential Manager /
//! macOS Keychain / libsecret) would be the upgrade if this crate ever
//! needs a stronger guarantee.
//!
//! Same OS-data-dir convention this project already uses everywhere
//! (`dirs::data_dir()`, e.g. `ls_security::peers`'s `known_peers.json`,
//! `ls_containers`'s `provisioning.log`):
//! `{OS data dir}/localsync/google_tokens.json`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::oauth::TokenSet;

pub(crate) fn default_dir() -> Result<PathBuf> {
    let base = dirs::data_dir().context("could not determine the OS data directory")?;
    Ok(base.join("localsync"))
}

/// Loads the stored token set, or `None` if nothing has been linked yet
/// (missing file - not an error, same pattern `ProvisioningLog`/
/// `KnownPeers` already use for their own files).
pub fn load_tokens() -> Result<Option<TokenSet>> {
    load_in(&default_dir()?)
}

pub fn save_tokens(tokens: &TokenSet) -> Result<()> {
    save_in(&default_dir()?, tokens)
}

/// Deletes the stored token set, for an eventual "unlink" action. Missing
/// file is not an error - already unlinked.
pub fn clear_tokens() -> Result<()> {
    clear_in(&default_dir()?)
}

const FILE_NAME: &str = "google_tokens.json";

/// `pub(crate)` rather than test-only: `oauth::ensure_valid_access_token`
/// (same crate, different module) also needs this to load/save against the
/// real default dir, so it can't be gated to `#[cfg(test)]` the way
/// `KnownPeers::load_in` is (there, the only other-module caller was
/// `load_default` in the same file). Tests below use it directly to point
/// at a temp dir instead of the real OS data dir.
pub(crate) fn load_in(dir: &Path) -> Result<Option<TokenSet>> {
    let path = dir.join(FILE_NAME);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let tokens = serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", path.display()))?;
            Ok(Some(tokens))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub(crate) fn save_in(dir: &Path, tokens: &TokenSet) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(FILE_NAME);
    let bytes = serde_json::to_vec_pretty(tokens)?;
    write_private(&path, &bytes)
}

pub(crate) fn clear_in(dir: &Path) -> Result<()> {
    let path = dir.join(FILE_NAME);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

/// Writes `bytes` to `path`, restricted to `0600` on Unix immediately on
/// creation (matches `ls-snapshot::sign::load_or_create_identity`'s
/// identity.key precedent) - plaintext tokens on disk are exactly the kind
/// of file that should never briefly exist world-readable.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting permissions on {}", path.display()))?;
    }
    Ok(())
}

/// Small helper only used by `retention.rs`'s sibling store, kept here so
/// both share one "write JSON, 0600 on Unix" implementation instead of
/// duplicating it.
pub(crate) fn write_json_private<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    write_private(path, &bytes)
}

pub(crate) fn read_json_or_default<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> Result<T> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::TokenSet;

    fn sample_tokens() -> TokenSet {
        TokenSet {
            access_token: "access-123".into(),
            refresh_token: Some("refresh-456".into()),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
            scopes: vec!["openid".into(), "email".into()],
            email: "user@example.com".into(),
        }
    }

    #[test]
    fn missing_file_is_none_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_in(dir.path()).unwrap().is_none());
    }

    #[test]
    fn round_trips_through_a_real_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = sample_tokens();
        save_in(dir.path(), &tokens).unwrap();

        let reloaded = load_in(dir.path()).unwrap().expect("should be present after save");
        assert_eq!(reloaded, tokens);
        assert!(dir.path().join(FILE_NAME).is_file());
    }

    #[test]
    fn clear_removes_the_file_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        save_in(dir.path(), &sample_tokens()).unwrap();
        assert!(load_in(dir.path()).unwrap().is_some());

        clear_in(dir.path()).unwrap();
        assert!(load_in(dir.path()).unwrap().is_none());
        // Clearing an already-clear store is not an error.
        clear_in(dir.path()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        save_in(dir.path(), &sample_tokens()).unwrap();
        let mode = std::fs::metadata(dir.path().join(FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
