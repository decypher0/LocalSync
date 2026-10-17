//! Files outside the project that a config value points at (an absolute path
//! in `application.properties`, say). Detection is read-only; the chosen
//! files travel in the snapshot under `external/<id>/`.

use serde::{Deserialize, Serialize};

pub use ls_snapshot::ExternalKind;

/// Where an absolute path found in a config file is, on this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalStatus {
    /// Inside the project: a relative path would do.
    Inside,
    /// Outside the project and present: can be bundled.
    OutsideExists,
    /// Outside the project and missing here: can't be bundled.
    OutsideMissing,
}

/// One active config value that is an absolute filesystem path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExternalRef {
    pub id: String,
    pub key: String,
    /// Relative to the project root, `/`-separated.
    pub config_file: String,
    /// The value exactly as the config file writes it.
    pub value: String,
    /// The path on this computer the value resolves to.
    pub path: String,
    pub kind: ExternalKind,
    pub status: ExternalStatus,
    #[serde(default)]
    pub size_bytes: u64,
    /// For [`ExternalStatus::Inside`]: the same path relative to the project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_relative: Option<String>,
}

/// An active `key = absolute path` line in one config file's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathHit {
    pub key: String,
    pub value: String,
    /// 1-based.
    pub line: usize,
}

/// Active (not commented-out) values in `text` that are absolute
/// filesystem paths. `config_file` picks the syntax (.properties, .yml,
/// .env, .xml). Agent D implements this; compat.rs calls it.
pub fn find_absolute_paths(_config_file: &str, _text: &str) -> Vec<PathHit> {
    Vec::new()
}

/// Scans a project folder's config files. Agent D implements this.
#[tauri::command]
pub async fn detect_external_refs(_folder_path: String) -> Result<Vec<ExternalRef>, String> {
    Ok(Vec::new())
}
