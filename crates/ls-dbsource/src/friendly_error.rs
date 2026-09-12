//! Round 22: real click-through against genuine local setups found that
//! round 18's error-chain fix (`{:#}`, not `.to_string()`) — while it does
//! preserve the real underlying reason instead of swallowing it — still
//! surfaces raw driver internals verbatim: the `mysql` crate's own error
//! enum variant name (`DriverError { ... }`) leaking through Debug-style
//! formatting reads, to a developer glancing at it, like "the driver is
//! missing" (it isn't - it's just that enum's name for "an error from the
//! driver"); the `mongodb` crate's connection-refused case is far worse,
//! dumping its entire internal topology structure
//! (`Kind: Server selection timeout: ... Topology: { Type: Single, Servers:
//! [ { ... labels: {...}, source: None, server response: None } ] },
//! labels: {}, source: None, server response: None`) - real output,
//! captured directly against a real unreachable port in this sandbox, not
//! guessed at. A third, separate real finding: `tokio-postgres`'s `Error`
//! Display for a `Kind::Db` failure (a real server-side rejection, e.g. a
//! wrong password) is just the bare string "db error" - the actual
//! message ("password authentication failed for user ...") only lives one
//! level down, in that error's `.source()` (`DbError::message()`) -
//! confirmed directly against a real wrong-password failure in this
//! sandbox, not assumed.
//!
//! This module doesn't hide any of that (round 18's whole point stands:
//! nothing gets swallowed) - it walks the *full* `std::error::Error`
//! source chain (via `anyhow::Error::new(err)`'s own `{:#}`, which does
//! this for any `source()`-implementing error, not just anyhow-added
//! contexts) and prepends one clear, human sentence in front of it, so the
//! sentence most people will actually read is useful, and the full,
//! walked-out chain (not just one driver's top-level Display) is still
//! right there for anyone who wants it.

/// Recognizes a handful of common failure shapes across all three engines'
/// underlying driver error text (matched case-insensitively, since the
/// exact casing/wording differs per driver) and returns a short, human
/// summary for it - `None` for anything not recognized, so an unfamiliar
/// error is never mislabeled, just left to speak for itself via the full
/// chain that always follows it.
pub fn summarize(raw: &str) -> Option<&'static str> {
    let lower = raw.to_lowercase();
    if lower.contains("connection refused") {
        Some("No database server appears to be listening at that host and port")
    } else if lower.contains("name or service not known")
        || lower.contains("failed to lookup address")
        || lower.contains("nodename nor servname")
        || lower.contains("no such host")
    {
        Some("That hostname could not be resolved")
    } else if lower.contains("server selection timeout") || lower.contains("timed out") || lower.contains("timeout") {
        Some("The connection attempt timed out")
    } else if lower.contains("access denied") || lower.contains("authentication failed") || lower.contains("password authentication failed")
    {
        Some("The server rejected the username/password")
    } else if lower.contains("unknown database") || (lower.contains("database") && lower.contains("does not exist")) {
        Some("That database/schema was not found on the server")
    } else {
        None
    }
}

/// Builds the actual error a `connect`/`export` caller sees from an
/// already-flattened `chained` string: the human summary (if recognized)
/// first, then who/where/what was being attempted, then the complete raw
/// text - so the technical detail round 18's fix already fought to
/// preserve is still fully present, just no longer the only thing on
/// screen. `pub(crate)` (not `pub`): real callers should go through
/// [`connect_failure`] below, which does the chain-walking for them; this
/// is exposed only so this module's own tests can supply pre-built chain
/// text directly without needing a real `std::error::Error` value.
pub(crate) fn build(username: &str, host: &str, port: u16, database: &str, chained: &str) -> anyhow::Error {
    match summarize(chained) {
        Some(summary) => anyhow::anyhow!("{summary} ({username}@{host}:{port}/{database}) - {chained}"),
        None => anyhow::anyhow!("failed to connect to {username}@{host}:{port}/{database}: {chained}"),
    }
}

/// The real entry point every engine's connect path uses. Takes the raw
/// driver error directly - anything implementing `std::error::Error` - so
/// its *full* `.source()` chain gets walked (via `anyhow::Error::new(err)`'s
/// own `{:#}`, not just `err.to_string()`), not only its own top-level
/// `Display`, which for some drivers is uselessly vague on its own (see
/// this module's own doc comment for `tokio-postgres`'s real "db error"
/// case).
pub fn connect_failure<E>(username: &str, host: &str, port: u16, database: &str, err: E) -> anyhow::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    let chained = format!("{:#}", anyhow::Error::new(err));
    build(username, host, port, database, &chained)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_connection_refused() {
        assert_eq!(
            summarize("DriverError { Could not connect to address `127.0.0.1:3306': Connection refused (os error 111) }"),
            Some("No database server appears to be listening at that host and port")
        );
    }

    #[test]
    fn recognizes_unresolvable_host() {
        assert_eq!(
            summarize("failed to lookup address information: Name or service not known"),
            Some("That hostname could not be resolved")
        );
    }

    #[test]
    fn recognizes_mongodb_server_selection_timeout_verbatim_dump() {
        // Real captured text (see module doc comment) - proves the ugly,
        // sprawling mongodb topology dump still gets a clean summary. This
        // one genuinely nests a real "Connection refused" *inside* the
        // driver's own "Server selection timeout" wrapper (it retries
        // server discovery for a while before giving up and reporting
        // that outer timeout) - "connection refused" is checked first and
        // is the more accurate, specific summary of the two real
        // candidates actually present in this text, not just whichever
        // happened to match first.
        let raw = "Kind: Server selection timeout: No available servers. Topology: { Type: Single, Servers: [ { Address: 127.0.0.1:33997, Type: Unknown, Error: Kind: I/O error: Connection refused (os error 111), labels: {\"RetryableError\"}, source: None, server response: None } ] }, labels: {}, source: None, server response: None";
        assert_eq!(summarize(raw), Some("No database server appears to be listening at that host and port"));
    }

    #[test]
    fn recognizes_a_genuine_timeout_with_no_connection_refused_present() {
        // Unlike the case above, a real network-level timeout (packets
        // dropped by a firewall, nothing replying at all) never mentions
        // "connection refused" anywhere - this is what the "timed out"
        // branch is actually for.
        assert_eq!(
            summarize("Kind: Server selection timeout: No available servers. Topology: { Type: Single, Servers: [] }"),
            Some("The connection attempt timed out")
        );
    }

    #[test]
    fn recognizes_auth_failure() {
        assert_eq!(
            summarize("MySqlError { ERROR 1045 (28000): Access denied for user 'root'@'localhost' (using password: YES) }"),
            Some("The server rejected the username/password")
        );
        assert_eq!(summarize("password authentication failed for user \"postgres\""), Some("The server rejected the username/password"));
    }

    #[test]
    fn recognizes_postgres_real_db_error_wrapper_via_its_source_chain() {
        // Real, confirmed behavior (see module doc comment): tokio-postgres's
        // own Display for this case is just "db error" - the real message
        // lives in its source(). `build` here takes the already-chained
        // text the way `connect_failure` would produce it from a real
        // error ("db error: password authentication failed for user ...").
        let chained = "db error: password authentication failed for user \"postgres\"";
        assert_eq!(summarize(chained), Some("The server rejected the username/password"));
    }

    #[test]
    fn recognizes_unknown_database() {
        assert_eq!(
            summarize("MySqlError { ERROR 1049 (42000): Unknown database 'xusom' }"),
            Some("That database/schema was not found on the server")
        );
    }

    #[test]
    fn unrecognized_text_gets_no_summary_but_is_not_lost() {
        assert_eq!(summarize("some totally novel driver error nobody has seen before"), None);
        let err = build("root", "127.0.0.1", 3306, "db", "some totally novel driver error nobody has seen before");
        let text = format!("{err:#}");
        assert!(text.contains("some totally novel driver error nobody has seen before"));
    }

    #[test]
    fn connect_failure_always_keeps_the_full_raw_text() {
        let err = build("root", "127.0.0.1", 3306, "somedb", "Connection refused (os error 111)");
        let text = format!("{err:#}");
        assert!(text.contains("No database server appears to be listening"));
        assert!(text.contains("root@127.0.0.1:3306/somedb"));
        assert!(text.contains("Connection refused (os error 111)"), "raw text must survive: {text}");
    }

    #[test]
    fn connect_failure_walks_a_real_std_error_source_chain() {
        #[derive(Debug)]
        struct Inner;
        impl std::fmt::Display for Inner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "password authentication failed for user \"postgres\"")
            }
        }
        impl std::error::Error for Inner {}

        #[derive(Debug)]
        struct Outer(Inner);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                // Mirrors tokio-postgres's real Kind::Db Display exactly:
                // uselessly vague on its own, real detail one level down.
                write!(f, "db error")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let err = connect_failure("postgres", "127.0.0.1", 5432, "src_db", Outer(Inner));
        let text = format!("{err:#}");
        assert!(
            text.contains("password authentication failed"),
            "the real reason, one level down in source(), must survive: {text}"
        );
        assert!(text.contains("The server rejected the username/password"));
    }
}
