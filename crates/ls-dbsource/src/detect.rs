//! Static, file-only detection of a Spring Boot project's MySQL/MariaDB
//! connection details from its own config files.
//!
//! Deliberately does **not** attempt a live connection - that's
//! [`crate::connect`]'s job. This module only reads and parses text files
//! that are presumably already sitting on disk in the developer's project.

use crate::types::{ConnectionDetails, DetectedConnection};
use std::fs;
use std::path::Path;

/// Extension point: today only Spring Boot's config format is implemented
/// and tested (this project's real case), but this trait lets a future
/// round add e.g. a `.env`-file detector without touching `detect_all`'s
/// call site.
pub trait ConfigDetector {
    fn detect(&self, folder: &Path) -> Option<DetectedConnection>;
}

pub struct SpringBootDetector;

/// Candidate config files, in search order. The standard Maven/Gradle
/// Spring Boot layout (`src/main/resources/...`) is tried first since it
/// matches the real project this feature targets; the folder-root variants
/// are a fallback for non-standard layouts.
const CANDIDATES: &[&str] = &[
    "src/main/resources/application.properties",
    "src/main/resources/application.yml",
    "src/main/resources/application.yaml",
    "application.properties",
    "application.yml",
    "application.yaml",
];

impl ConfigDetector for SpringBootDetector {
    fn detect(&self, folder: &Path) -> Option<DetectedConnection> {
        for candidate in CANDIDATES {
            let path = folder.join(candidate);
            if !path.is_file() {
                continue;
            }

            let details = if candidate.ends_with(".properties") {
                parse_properties_file(&path)
            } else {
                parse_yaml_file(&path)
            };

            if let Some(details) = details {
                // Best-effort canonicalization: the file is known to exist
                // at this point, so this only fails in exotic environments
                // (e.g. permission issues mid-race); fall back to the
                // joined path rather than losing the detection entirely.
                let source_file = fs::canonicalize(&path).unwrap_or(path);
                return Some(DetectedConnection {
                    details,
                    source_file,
                });
            }
            // Extraction failed/incomplete for this file: don't fall back
            // to a partial result, but do keep trying the remaining
            // candidates rather than giving up entirely.
        }
        None
    }
}

/// Tries every registered detector (currently just [`SpringBootDetector`])
/// in order and returns the first successful detection.
pub fn detect_all(folder: &Path) -> Option<DetectedConnection> {
    let detectors: Vec<Box<dyn ConfigDetector>> = vec![Box::new(SpringBootDetector)];
    detectors.iter().find_map(|d| d.detect(folder))
}

/// Parses a Java `.properties` file just far enough to pull out Spring
/// Boot's `spring.datasource.*` keys.
///
/// Deliberately simple: supports `key=value`, `key:value`, and
/// `key = value` (whitespace around the separator is trimmed), `#`/`!`
/// comment lines, and blank lines. Does **not** support line continuations
/// (`\` at end of line) or `\uXXXX` unicode escapes - this project's real
/// case doesn't use either, so the parser stays a straightforward
/// line-at-a-time scan rather than a full properties-format implementation.
fn parse_properties_file(path: &Path) -> Option<ConnectionDetails> {
    let contents = fs::read_to_string(path).ok()?;

    let mut url: Option<String> = None;
    let mut username: Option<String> = None;
    let mut password: Option<String> = None;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
            continue;
        }

        let sep_index = trimmed
            .find('=')
            .into_iter()
            .chain(trimmed.find(':'))
            .min();
        let Some(sep_index) = sep_index else {
            continue;
        };

        let key = trimmed[..sep_index].trim();
        let value = trimmed[sep_index + 1..].trim();

        match key {
            "spring.datasource.url" => url = Some(value.to_string()),
            "spring.datasource.username" => username = Some(value.to_string()),
            "spring.datasource.password" => password = Some(value.to_string()),
            _ => {}
        }
    }

    build_connection_details(url, username, password)
}

/// Parses a Spring Boot `application.yml`/`.yaml` file, pulling
/// `spring.datasource.*` out of the nested mapping. Any of those keys
/// present but not a plain YAML string is treated as "not found" rather
/// than causing a panic.
fn parse_yaml_file(path: &Path) -> Option<ConnectionDetails> {
    let contents = fs::read_to_string(path).ok()?;
    let root: serde_yaml::Value = serde_yaml::from_str(&contents).ok()?;

    let datasource = yaml_get(&root, "spring").and_then(|s| yaml_get(s, "datasource"));

    let url = datasource.and_then(|d| yaml_get(d, "url")).and_then(yaml_as_str);
    let username = datasource
        .and_then(|d| yaml_get(d, "username"))
        .and_then(yaml_as_str);
    let password = datasource
        .and_then(|d| yaml_get(d, "password"))
        .and_then(yaml_as_str);

    build_connection_details(url, username, password)
}

/// Looks up a string key in a YAML mapping node, if the node is a mapping.
fn yaml_get<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a serde_yaml::Value> {
    value.as_mapping()?.get(serde_yaml::Value::String(key.to_string()))
}

/// Extracts a plain YAML string, treating any other value type (mapping,
/// sequence, number, bool, null) as "not found" rather than panicking.
fn yaml_as_str(value: &serde_yaml::Value) -> Option<String> {
    value.as_str().map(str::to_string)
}

/// Combines the raw `url`/`username`/`password` strings pulled out of a
/// config file into a validated [`ConnectionDetails`]. Missing/malformed
/// `url` or missing `username` fails the whole detection for this file;
/// missing `password` is legitimate (some local MySQL setups use none) and
/// becomes an empty string.
fn build_connection_details(
    url: Option<String>,
    username: Option<String>,
    password: Option<String>,
) -> Option<ConnectionDetails> {
    let (engine, host, port, database) = parse_jdbc_url(&url?)?;
    let username = username?;
    let password = password.unwrap_or_default();

    Some(ConnectionDetails {
        engine,
        host,
        port,
        database,
        username,
        password,
    })
}

/// Parses a `spring.datasource.url` JDBC URL into `(engine, host, port,
/// database)`.
///
/// Accepts `jdbc:mysql://...` and `jdbc:mariadb://...` - both map to
/// `engine = "mysql"` in the returned [`ConnectionDetails`]. This is a
/// deliberate simplification, not a bug: MariaDB is wire-compatible with
/// MySQL, and this project's live-connection code ([`crate::connect`])
/// only implements a single "mysql" engine that speaks that wire protocol
/// against either server.
///
/// Any other scheme, or a URL missing a host or a database path segment,
/// returns `None` rather than guessing. An omitted port defaults to 3306.
fn parse_jdbc_url(url: &str) -> Option<(String, String, u16, String)> {
    let rest = if let Some(r) = url.strip_prefix("jdbc:mysql://") {
        r
    } else if let Some(r) = url.strip_prefix("jdbc:mariadb://") {
        r
    } else {
        return None;
    };

    let (host_port, path) = rest.split_once('/')?;
    if host_port.is_empty() {
        return None;
    }

    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => {
            if h.is_empty() {
                return None;
            }
            let port: u16 = p.parse().ok()?;
            (h.to_string(), port)
        }
        None => (host_port.to_string(), 3306),
    };

    // Strip a trailing `?key=value&...` query string before taking the
    // database name.
    let database = path.split('?').next().unwrap_or("");
    if database.is_empty() {
        return None;
    }

    Some(("mysql".to_string(), host, port, database.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn detects_from_standard_layout_properties_file() {
        let dir = tempdir().unwrap();
        let file = dir
            .path()
            .join("src/main/resources/application.properties");
        write_file(
            &file,
            "spring.datasource.url=jdbc:mysql://localhost:3306/mydb\n\
             spring.datasource.username=root\n\
             spring.datasource.password=secret\n",
        );

        let detected = detect_all(dir.path()).expect("expected a detection");
        assert_eq!(detected.details.engine, "mysql");
        assert_eq!(detected.details.host, "localhost");
        assert_eq!(detected.details.port, 3306);
        assert_eq!(detected.details.database, "mydb");
        assert_eq!(detected.details.username, "root");
        assert_eq!(detected.details.password, "secret");

        let expected_source = fs::canonicalize(&file).unwrap();
        assert_eq!(detected.source_file, expected_source);
    }

    #[test]
    fn detects_from_standard_layout_yaml_file_and_strips_query_params() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("src/main/resources/application.yml");
        write_file(
            &file,
            "spring:\n\
             \x20\x20datasource:\n\
             \x20\x20\x20\x20url: jdbc:mysql://localhost:3306/mydb?useSSL=false&serverTimezone=UTC\n\
             \x20\x20\x20\x20username: root\n\
             \x20\x20\x20\x20password: secret\n",
        );

        let detected = detect_all(dir.path()).expect("expected a detection");
        assert_eq!(detected.details.engine, "mysql");
        assert_eq!(detected.details.host, "localhost");
        assert_eq!(detected.details.port, 3306);
        assert_eq!(detected.details.database, "mydb");
        assert_eq!(detected.details.username, "root");
        assert_eq!(detected.details.password, "secret");

        let expected_source = fs::canonicalize(&file).unwrap();
        assert_eq!(detected.source_file, expected_source);
    }

    #[test]
    fn missing_password_key_becomes_empty_string() {
        let dir = tempdir().unwrap();
        let file = dir
            .path()
            .join("src/main/resources/application.properties");
        write_file(
            &file,
            "spring.datasource.url=jdbc:mysql://localhost:3306/mydb\n\
             spring.datasource.username=root\n",
        );

        let detected = detect_all(dir.path()).expect("expected a detection");
        assert_eq!(detected.details.password, "");
    }

    #[test]
    fn non_mysql_jdbc_url_yields_no_detection() {
        let dir = tempdir().unwrap();
        let file = dir
            .path()
            .join("src/main/resources/application.properties");
        write_file(
            &file,
            "spring.datasource.url=jdbc:postgresql://localhost:5432/mydb\n\
             spring.datasource.username=root\n",
        );

        assert!(detect_all(dir.path()).is_none());
    }

    #[test]
    fn jdbc_url_without_explicit_port_defaults_to_3306() {
        let dir = tempdir().unwrap();
        let file = dir
            .path()
            .join("src/main/resources/application.properties");
        write_file(
            &file,
            "spring.datasource.url=jdbc:mysql://localhost/mydb\n\
             spring.datasource.username=root\n",
        );

        let detected = detect_all(dir.path()).expect("expected a detection");
        assert_eq!(detected.details.port, 3306);
        assert_eq!(detected.details.host, "localhost");
        assert_eq!(detected.details.database, "mydb");
    }

    #[test]
    fn empty_folder_yields_no_detection() {
        let dir = tempdir().unwrap();
        assert!(detect_all(dir.path()).is_none());
    }

    #[test]
    fn properties_comments_and_blank_lines_are_ignored() {
        let dir = tempdir().unwrap();
        let file = dir
            .path()
            .join("src/main/resources/application.properties");
        write_file(
            &file,
            "# this is a comment\n\
             \n\
             ! this is also a comment\n\
             spring.datasource.url=jdbc:mysql://localhost:3306/mydb\n\
             \n\
             # spring.datasource.username=wrong\n\
             spring.datasource.username=root\n\
             spring.datasource.password=secret\n",
        );

        let detected = detect_all(dir.path()).expect("expected a detection");
        assert_eq!(detected.details.username, "root");
        assert_eq!(detected.details.password, "secret");
    }

    #[test]
    fn falls_through_to_next_candidate_when_first_found_file_fails_to_parse() {
        let dir = tempdir().unwrap();
        // Standard-layout properties file exists but has an unsupported
        // JDBC scheme - should not produce a partial/wrong result, and
        // should not stop the search entirely.
        let bad_file = dir
            .path()
            .join("src/main/resources/application.properties");
        write_file(
            &bad_file,
            "spring.datasource.url=jdbc:postgresql://localhost:5432/mydb\n\
             spring.datasource.username=root\n",
        );

        // Root-level fallback file has a valid config.
        let good_file = dir.path().join("application.yml");
        write_file(
            &good_file,
            "spring:\n\
             \x20\x20datasource:\n\
             \x20\x20\x20\x20url: jdbc:mysql://localhost:3306/mydb\n\
             \x20\x20\x20\x20username: root\n\
             \x20\x20\x20\x20password: secret\n",
        );

        let detected = detect_all(dir.path()).expect("expected a detection");
        assert_eq!(detected.details.database, "mydb");
        let expected_source = fs::canonicalize(&good_file).unwrap();
        assert_eq!(detected.source_file, expected_source);
    }
}
