//! A receiver-side, purely local record of "pubkeys I've seen before and
//! what I've named them" — the same idea as SSH's `known_hosts` or a
//! Bluetooth pairing list. The sender never sees or sends a name; the
//! receiver assigns one locally to a pubkey it already receives today via
//! `Manifest::sender_pubkey`.
//!
//! This is bookkeeping only. Recognizing a peer here must never substitute
//! for [`crate::verify`] (which still runs exactly as before, on every
//! receive) and must never skip or shortcut the diff-review-then-Run gate —
//! see `commands::receive_snapshot` / `commands::run_snapshot` in the
//! desktop app, which stay two structurally separate steps.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub pubkey: [u8; 32],
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub first_seen: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen: time::OffsetDateTime,
}

/// A handful of entries at most in real use — a `Vec` scanned linearly is
/// plenty; no need for a `HashMap` keyed by pubkey at this scale.
#[derive(Debug)]
pub struct KnownPeers {
    peers: Vec<PeerInfo>,
    path: PathBuf,
}

impl KnownPeers {
    /// `{OS data dir}/localsync/known_peers.json` — same convention
    /// `ls_containers::ProvisioningLog` uses. Missing file = empty
    /// `KnownPeers` (first run), not an error.
    pub fn load_default() -> Result<Self> {
        let base = dirs::data_dir().context("could not determine the OS data directory")?;
        Self::load_in(&base.join("localsync"))
    }

    /// Test-only variant of `load_default` that reads/writes under a
    /// caller-chosen directory instead of the real OS data dir. Same pattern
    /// `ProvisioningLog::open_in` uses.
    #[cfg(test)]
    pub fn load_in(dir: &Path) -> Result<Self> {
        Self::load_in_impl(dir)
    }

    #[cfg(not(test))]
    fn load_in(dir: &Path) -> Result<Self> {
        Self::load_in_impl(dir)
    }

    fn load_in_impl(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join("known_peers.json");
        let peers = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self { peers, path })
    }

    pub fn find(&self, pubkey: &[u8; 32]) -> Option<&PeerInfo> {
        self.peers.iter().find(|p| &p.pubkey == pubkey)
    }

    /// Inserts a new peer, or updates the name and bumps `last_seen` if
    /// `pubkey` is already known. Write-through: persists to disk before
    /// returning, no separate save call needed.
    pub fn remember(&mut self, pubkey: [u8; 32], name: String) -> Result<()> {
        let now = time::OffsetDateTime::now_utc();
        match self.peers.iter_mut().find(|p| p.pubkey == pubkey) {
            Some(p) => {
                p.name = name;
                p.last_seen = now;
            }
            None => self.peers.push(PeerInfo { pubkey, name, first_seen: now, last_seen: now }),
        }
        self.save()
    }

    fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.peers)?;
        std::fs::write(&self.path, bytes).with_context(|| format!("writing {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    #[test]
    fn find_is_none_before_remember_and_some_after() {
        let dir = tempfile::tempdir().unwrap();
        let mut peers = KnownPeers::load_in(dir.path()).unwrap();
        assert!(peers.find(&key(1)).is_none());

        peers.remember(key(1), "Alice's laptop".into()).unwrap();
        let found = peers.find(&key(1)).expect("should be found after remember");
        assert_eq!(found.name, "Alice's laptop");
        assert_eq!(found.first_seen, found.last_seen);
    }

    #[test]
    fn remember_again_updates_not_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let mut peers = KnownPeers::load_in(dir.path()).unwrap();
        peers.remember(key(2), "first name".into()).unwrap();
        let first_seen = peers.find(&key(2)).unwrap().first_seen;

        peers.remember(key(2), "renamed".into()).unwrap();
        assert_eq!(peers.peers.len(), 1, "re-remembering the same pubkey must update, not duplicate");
        let updated = peers.find(&key(2)).unwrap();
        assert_eq!(updated.name, "renamed");
        assert_eq!(updated.first_seen, first_seen, "first_seen must not change on update");
    }

    #[test]
    fn round_trips_through_a_real_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut peers = KnownPeers::load_in(dir.path()).unwrap();
            peers.remember(key(3), "bob".into()).unwrap();
            peers.remember(key(4), "carol".into()).unwrap();
        }

        // Fresh load from the same dir — proves persistence, not just
        // in-memory state.
        let reloaded = KnownPeers::load_in(dir.path()).unwrap();
        assert_eq!(reloaded.find(&key(3)).unwrap().name, "bob");
        assert_eq!(reloaded.find(&key(4)).unwrap().name, "carol");
        assert!(dir.path().join("known_peers.json").is_file());
    }

    #[test]
    fn missing_file_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let peers = KnownPeers::load_in(dir.path()).unwrap();
        assert!(peers.find(&key(9)).is_none());
    }
}
