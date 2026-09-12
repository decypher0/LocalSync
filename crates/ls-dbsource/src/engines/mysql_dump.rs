//! Turns a developer-selected set of tables from a real, live database into
//! a real, non-sampled SQL dump: `CREATE TABLE` (via the server's own
//! `SHOW CREATE TABLE`, so it's byte-for-byte what the server would produce)
//! plus one `INSERT` per row, every row, every column - no `LIMIT`, no
//! sampling, no truncation. The caller (the Send wizard's command layer)
//! decides where the resulting bytes land on disk; this module only knows
//! how to talk to "a database" and produce SQL text.

use super::mysql::open_connection;
use crate::types::ConnectionDetails;
use anyhow::{bail, Context, Result};
use mysql::consts::ColumnType;
use mysql::prelude::Queryable;
use mysql::{Column, Conn, Row, Value};
use std::fmt::Write as _;

/// MySQL's "binary" character set id. A text-protocol column reports this
/// exact charset when (and only when) its underlying type is
/// BINARY/VARBINARY/BLOB/TINYBLOB/MEDIUMBLOB/LONGBLOB - i.e. "bytes with no
/// text encoding", as opposed to CHAR/VARCHAR/TEXT (which report a real
/// charset like utf8mb4 even when their *collation* happens to be a binary
/// one). It's the standard, reliable way client libraries distinguish "this
/// Bytes value is text" from "this Bytes value is opaque binary" over the
/// MySQL wire protocol, since both come back as the same `Value::Bytes`
/// variant.
const BINARY_CHARSET_ID: u16 = 63;

/// These names ultimately get backtick-quoted and interpolated directly
/// into SQL text (`` `<name>` ``), never bound as a query parameter. A
/// backtick inside the name would let it break out of that quoting, so this
/// is a real correctness/safety boundary, not decoration.
fn validate_identifier(name: &str) -> Result<()> {
    if name.contains('`') {
        bail!("table name '{name}' contains a backtick and cannot be safely quoted - refusing to export it");
    }
    Ok(())
}

/// Full (never sampled/limited) SQL dump of exactly `tables`, in the given
/// order: for each table, a `DROP TABLE IF EXISTS` + the server's own
/// `SHOW CREATE TABLE` text, followed by every row as an `INSERT`.
pub fn export_tables(details: &ConnectionDetails, tables: &[String]) -> Result<Vec<u8>> {
    if details.engine != "mysql" {
        bail!(
            "unsupported database engine '{}' - only \"mysql\" (MySQL/MariaDB) is implemented",
            details.engine
        );
    }

    let mut conn = open_connection(details)?;
    let mut out = String::new();

    for name in tables {
        validate_identifier(name)?;
        export_one_table(&mut conn, name, &mut out)?;
    }

    Ok(out.into_bytes())
}

fn export_one_table(conn: &mut Conn, name: &str, out: &mut String) -> Result<()> {
    writeln!(out, "-- Table: {name}").ok();
    writeln!(out, "DROP TABLE IF EXISTS `{name}`;").ok();

    let create: Option<(String, String)> = conn
        .query_first(format!("SHOW CREATE TABLE `{name}`"))
        .with_context(|| format!("failed to run SHOW CREATE TABLE for table '{name}'"))?;
    let (_, create_sql) = create
        .with_context(|| format!("table '{name}' does not exist (SHOW CREATE TABLE returned no result)"))?;
    writeln!(out, "{create_sql};").ok();

    let result = conn
        .query_iter(format!("SELECT * FROM `{name}`"))
        .with_context(|| format!("failed to query rows from table '{name}'"))?;

    let columns: Vec<Column> = result.columns().as_ref().to_vec();
    let column_names: Vec<String> = columns.iter().map(|c| c.name_str().into_owned()).collect();
    let is_binary: Vec<bool> = columns.iter().map(is_binary_column).collect();

    let quoted_columns = column_names
        .iter()
        .map(|c| format!("`{c}`"))
        .collect::<Vec<_>>()
        .join(",");

    for row_result in result {
        let row: Row =
            row_result.with_context(|| format!("failed reading a row from table '{name}'"))?;
        let mut values = Vec::with_capacity(row.len());
        for i in 0..row.len() {
            let value = row
                .as_ref(i)
                .with_context(|| format!("missing column {i} while reading a row of '{name}'"))?;
            values.push(format_value(value, is_binary.get(i).copied().unwrap_or(false)));
        }
        writeln!(
            out,
            "INSERT INTO `{name}` ({quoted_columns}) VALUES ({});",
            values.join(",")
        )
        .ok();
    }

    Ok(())
}

/// True if the server reports this column under the binary charset - see
/// [`BINARY_CHARSET_ID`]. Deliberately charset-based rather than
/// type-based: it's what actually determines whether a text-protocol
/// `Value::Bytes` for this column is opaque bytes or encoded text.
fn is_binary_column(column: &Column) -> bool {
    // Belt-and-braces: also treat the handful of geometry/bit types (which
    // never carry a meaningful text charset either) as binary, in case a
    // server/charset combination ever reports something other than 63 for
    // them.
    column.character_set() == BINARY_CHARSET_ID
        || matches!(column.column_type(), ColumnType::MYSQL_TYPE_GEOMETRY)
}

fn format_value(value: &Value, is_binary_col: bool) -> String {
    match value {
        Value::NULL => "NULL".to_string(),
        Value::Bytes(bytes) => {
            if is_binary_col {
                hex_literal(bytes)
            } else {
                match std::str::from_utf8(bytes) {
                    Ok(s) => format!("'{}'", escape_sql_string(s)),
                    // Not valid UTF-8 text (unexpected for a non-binary
                    // column, but possible with an unusual server charset).
                    // Fall back to a byte-exact hex literal rather than
                    // corrupting the (UTF-8) dump output or lossily
                    // replacing bytes.
                    Err(_) => hex_literal(bytes),
                }
            }
        }
        Value::Int(i) => i.to_string(),
        Value::UInt(u) => u.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Double(d) => d.to_string(),
        Value::Date(year, month, day, hour, minute, second, micros) => {
            format_date(*year, *month, *day, *hour, *minute, *second, *micros)
        }
        Value::Time(is_neg, days, hours, minutes, seconds, micros) => {
            format_time(*is_neg, *days, *hours, *minutes, *seconds, *micros)
        }
    }
}

fn hex_literal(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(3 + bytes.len() * 2);
    s.push_str("X'");
    for b in bytes {
        write!(s, "{b:02X}").ok();
    }
    s.push('\'');
    s
}

/// MySQL's default `sql_mode` treats `\` as an escape character in string
/// literals, so both it and the quote delimiter itself must be escaped.
/// Also escapes NUL/newline/CR/Ctrl+Z, the same set `mysqldump` escapes,
/// so control bytes that would otherwise corrupt the surrounding SQL text
/// (or a terminal replaying it) come through safely.
fn escape_sql_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\0' => out.push_str("\\0"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\x1a' => out.push_str("\\Z"),
            _ => out.push(c),
        }
    }
    out
}

fn format_date(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8, micros: u32) -> String {
    if hour == 0 && minute == 0 && second == 0 && micros == 0 {
        format!("'{year:04}-{month:02}-{day:02}'")
    } else if micros == 0 {
        format!("'{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}'")
    } else {
        format!("'{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{micros:06}'")
    }
}

fn format_time(is_neg: bool, days: u32, hours: u8, minutes: u8, seconds: u8, micros: u32) -> String {
    // MySQL's TIME range allows more than 24 hours, represented on the
    // wire as separate days+hours; folded back into one hour count for the
    // textual `HHH:MM:SS` form MySQL's own literal syntax expects.
    let total_hours = u64::from(days) * 24 + u64::from(hours);
    let sign = if is_neg { "-" } else { "" };
    if micros == 0 {
        format!("'{sign}{total_hours:02}:{minutes:02}:{seconds:02}'")
    } else {
        format!("'{sign}{total_hours:02}:{minutes:02}:{seconds:02}.{micros:06}'")
    }
}
