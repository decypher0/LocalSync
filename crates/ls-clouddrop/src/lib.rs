//! "Cloud drop" transport (round 23): send/receive a snapshot via the
//! sender's and receiver's own Google Drive, instead of a direct LAN
//! (round 8) or relay (round 10) connection. This crate is the OAuth +
//! Drive API integration only - no app wiring, no UI, no `ls-net` changes.
//!
//! Two findings this crate is built around (see each module's doc comments
//! for where):
//!
//! 1. `drive.file` scope only ever sees files *this app* created or the
//!    user explicitly picked - never a file merely shared to the signed-in
//!    user by someone else's app (confirmed against
//!    <https://developers.google.com/workspace/drive/api/guides/api-specific-auth>).
//!    A receiver reading a file the sender shared to them therefore needs
//!    `drive.readonly` too. Since every LocalSync instance can act as
//!    either sender or receiver, [`oauth`]'s linking flow requests the
//!    union of both up front: `openid email profile drive.file
//!    drive.readonly`.
//! 2. Drive's native `expirationTime` permission field only has real
//!    documented, enforced behavior on Google Workspace accounts; personal
//!    Gmail accounts are widely and consistently reported to silently drop
//!    it. [`drive::grant_reader_access`] always *attempts* to set it (free
//!    win on Workspace) but always reports back, honestly, whether Drive's
//!    response actually echoed it - see [`drive::GrantResult`] - and
//!    [`retention`] provides a full app-level fallback that does not depend
//!    on Drive's native mechanism working at all.
pub mod drive;
pub mod identity;
pub mod oauth;
pub mod retention;
pub mod store;
