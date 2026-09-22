//! Local-network peer discovery via mDNS (multicast DNS - the same
//! technology behind Bonjour/Avahi/Chromecast discovery), round 37. A thin
//! wrapper around the `mdns-sd` crate (actively maintained, handles the real
//! edge cases - name-conflict resolution, TTLs, service records - correctly
//! across Windows/macOS/Linux) rather than a hand-rolled UDP broadcast
//! protocol.
//!
//! This module only ever announces/discovers *presence* - a device's
//! nickname plus the address/room id a sender needs to reach it. It carries
//! no snapshot bytes and makes no trust decision on its own; see
//! `commands.rs`'s `ConnectionRequest`/`ConnectionResponse` handling (this
//! crate's `ControlMessage`) for where the actual accept/reject gate lives -
//! discovery only ever replaces a manual code-copy-paste step with a click.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceInfo};

/// One mDNS service type for every LocalSync instance that's opted into
/// discoverability - `_localsync._tcp.local.` follows the standard
/// `_service._proto.local.` naming convention mDNS/DNS-SD expects.
pub const SERVICE_TYPE: &str = "_localsync._tcp.local.";

/// A live, currently-discoverable LocalSync instance - everything a sender
/// needs to connect to it directly, reusing the exact "host an ephemeral
/// relay, connect a peer to it" mechanism `start_send_session`'s local mode
/// already uses (see `commands::set_discoverable`, the receiver-side
/// counterpart that produces the values announced here).
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredPeer {
    /// mDNS's own per-instance identifier - needed to tell "the same device,
    /// still here" apart from "a different instance that happens to share a
    /// nickname" across repeated browse events.
    pub fullname: String,
    pub nickname: String,
    pub host: Ipv4Addr,
    pub port: u16,
    pub room_id: String,
}

/// Starts advertising this device's presence. Returns the daemon (must be
/// kept alive for as long as discoverability stays on - dropping/shutting it
/// down stops the advertisement) and the service's fullname (needed to
/// unregister it later, via [`stop_announcing`]).
pub fn announce(nickname: &str, host: Ipv4Addr, port: u16, room_id: &str) -> Result<(ServiceDaemon, String)> {
    let daemon = ServiceDaemon::new().context("failed to start the mDNS daemon")?;
    // The room id (already a short, per-session-random alnum id - see
    // generate_room_id) doubles as a cheap, good-enough-for-one-LAN unique
    // instance suffix, so two LocalSync instances announcing at once (e.g.
    // two windows on the same box, for same-box testing) never collide on
    // instance name.
    let instance_name = format!("localsync-{room_id}");
    let host_name = format!("{instance_name}.local.");
    let mut properties = HashMap::new();
    properties.insert("name".to_string(), nickname.to_string());
    properties.insert("room".to_string(), room_id.to_string());
    let service = ServiceInfo::new(SERVICE_TYPE, &instance_name, &host_name, IpAddr::V4(host), port, properties)
        .context("failed to build the mDNS service record")?;
    let fullname = service.get_fullname().to_string();
    daemon.register(service).map_err(|e| anyhow::anyhow!("failed to announce on mDNS: {e}"))?;
    Ok((daemon, fullname))
}

/// Stops advertising and shuts the daemon down. Best-effort: a failure here
/// just means the announcement lingers until its TTL naturally expires
/// (mDNS's own designed-in self-healing for exactly this case), not a
/// reason to fail the toggle-off action itself - logged, not propagated.
pub fn stop_announcing(daemon: &ServiceDaemon, fullname: &str) {
    if let Err(e) = daemon.unregister(fullname) {
        log::warn!("mdns: failed to unregister {fullname}: {e}");
    }
    if let Err(e) = daemon.shutdown() {
        log::warn!("mdns: failed to shut down the announce daemon: {e}");
    }
}

/// The event stream [`start_browsing`] returns - named so callers outside
/// this crate never need `mdns-sd` as a direct dependency just to spell out
/// the type of a variable/parameter (see this crate's own `Cargo.toml`
/// comment on why `mdns-sd` stays ls-net-only).
pub type DiscoveryEvents = mdns_sd::Receiver<mdns_sd::ServiceEvent>;

/// Starts browsing for other discoverable LocalSync instances. Returns the
/// daemon (must be kept alive for as long as browsing should continue) and
/// the raw event stream - see [`resolved_peer`] to turn a
/// `ServiceEvent::ServiceResolved` into a [`DiscoveredPeer`]; a
/// `ServiceEvent::ServiceRemoved(_, fullname)` event's `fullname` is what
/// identifies which previously-resolved peer just went away.
pub fn start_browsing() -> Result<(ServiceDaemon, DiscoveryEvents)> {
    let daemon = ServiceDaemon::new().context("failed to start the mDNS daemon")?;
    let receiver = daemon
        .browse(SERVICE_TYPE)
        .map_err(|e| anyhow::anyhow!("failed to start mDNS browsing: {e}"))?;
    Ok((daemon, receiver))
}

/// Best-effort, same reasoning as [`stop_announcing`].
pub fn stop_browsing(daemon: &ServiceDaemon) {
    if let Err(e) = daemon.stop_browse(SERVICE_TYPE) {
        log::debug!("mdns: stop_browse failed (daemon may already be shutting down): {e}");
    }
    if let Err(e) = daemon.shutdown() {
        log::warn!("mdns: failed to shut down the browse daemon: {e}");
    }
}

/// A malformed/incomplete service record (missing the `name`/`room`
/// properties this app itself always sets on every announcement, or no
/// IPv4 address at all) is silently skipped rather than an error - mDNS is
/// a best-effort mechanism shared with the whole LAN; a stray non-LocalSync
/// record, or a half-announced one from an instance that's still starting
/// up, is expected, ordinary noise, not a bug in this app.
pub fn resolved_peer(resolved: &mdns_sd::ResolvedService) -> Option<DiscoveredPeer> {
    let nickname = resolved.get_property_val_str("name")?.to_string();
    let room_id = resolved.get_property_val_str("room")?.to_string();
    let host = resolved.get_addresses_v4().into_iter().next()?;
    Some(DiscoveredPeer { fullname: resolved.fullname.clone(), nickname, host, port: resolved.port, room_id })
}
