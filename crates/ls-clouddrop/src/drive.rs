//! Minimal Google Drive API v3 client for Cloud drop: create/find a
//! dedicated app folder, upload into it, grant one targeted reader
//! permission, download, delete, and list permissions.
//!
//! Deliberately narrow scope: uploads go into a folder this app itself
//! creates and never grants a permission at upload time - exactly what
//! `drive.file` scope covers (see the crate root doc comment for why that
//! scope can't see anything wider). Granting access is a separate,
//! explicit, later call ([`grant_reader_access`]), and it always creates a
//! *targeted* `type: "user"` permission for one specific email - never
//! `type: "anyone"`.

use anyhow::{Context, Result};
use serde::Deserialize;

const API_BASE: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD_BASE: &str = "https://www.googleapis.com/upload/drive/v3";
const APP_FOLDER_NAME: &str = "LocalSync Cloud Drop";

struct DriveEndpoints {
    api_base: String,
    upload_base: String,
}

impl Default for DriveEndpoints {
    fn default() -> Self {
        Self { api_base: API_BASE.to_string(), upload_base: UPLOAD_BASE.to_string() }
    }
}

#[derive(Debug, Deserialize)]
struct DriveErrorBody {
    error: DriveErrorDetail,
}

#[derive(Debug, Deserialize)]
struct DriveErrorDetail {
    message: String,
    #[serde(default)]
    status: Option<String>,
}

/// Turns a non-2xx Drive response into a descriptive `anyhow::Error`
/// instead of letting callers guess from a bare status code - real Drive
/// error bodies (`{"error":{"code":404,"message":"File not found: ..."}}`)
/// are parsed when present, with a sane fallback when they're not.
fn drive_error(status: reqwest::StatusCode, body: String, what: &str) -> anyhow::Error {
    match serde_json::from_str::<DriveErrorBody>(&body) {
        Ok(parsed) => anyhow::anyhow!(
            "Drive API error while {what} ({status}{}): {}",
            parsed.error.status.map(|s| format!(" {s}")).unwrap_or_default(),
            parsed.error.message
        ),
        Err(_) => anyhow::anyhow!("Drive API error while {what} ({status}): {body}"),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UploadedFile {
    pub file_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GrantResult {
    pub permission_id: String,
    pub expiration_requested: Option<time::OffsetDateTime>,
    /// What Drive's response actually reflected - **not** an echo of
    /// whether we merely requested one. `false` whenever
    /// `expiration_requested` was `None` (nothing to apply) or when Drive
    /// silently dropped the field, which is the documented real-world
    /// behavior for personal Gmail accounts (see crate root doc comment).
    pub expiration_applied: bool,
}

// ==================== app folder ====================

pub async fn get_or_create_app_folder(access_token: &str) -> Result<String> {
    get_or_create_app_folder_with(access_token, &DriveEndpoints::default()).await
}

async fn get_or_create_app_folder_with(access_token: &str, endpoints: &DriveEndpoints) -> Result<String> {
    #[derive(Deserialize)]
    struct FileRef {
        id: String,
    }
    #[derive(Deserialize)]
    struct ListResponse {
        files: Vec<FileRef>,
    }
    #[derive(Deserialize)]
    struct CreatedFile {
        id: String,
    }

    let client = reqwest::Client::new();
    let query = format!("mimeType='application/vnd.google-apps.folder' and name='{APP_FOLDER_NAME}' and trashed=false");
    let resp = client
        .get(format!("{}/files", endpoints.api_base))
        .bearer_auth(access_token)
        .query(&[("q", query.as_str()), ("fields", "files(id,name)"), ("spaces", "drive")])
        .send()
        .await
        .context("listing Drive files to find the Cloud drop app folder")?;
    let status = resp.status();
    let body_text = resp.text().await.context("reading file-list response")?;
    if !status.is_success() {
        return Err(drive_error(status, body_text, "listing files"));
    }
    let list: ListResponse =
        serde_json::from_str(&body_text).with_context(|| format!("parsing file-list response: {body_text}"))?;
    if let Some(existing) = list.files.into_iter().next() {
        return Ok(existing.id);
    }

    let create_resp = client
        .post(format!("{}/files", endpoints.api_base))
        .bearer_auth(access_token)
        .query(&[("fields", "id")])
        .json(&serde_json::json!({
            "name": APP_FOLDER_NAME,
            "mimeType": "application/vnd.google-apps.folder",
        }))
        .send()
        .await
        .context("creating the Cloud drop app folder")?;
    let status = create_resp.status();
    let body_text = create_resp.text().await.context("reading folder-create response")?;
    if !status.is_success() {
        return Err(drive_error(status, body_text, "creating app folder"));
    }
    let created: CreatedFile =
        serde_json::from_str(&body_text).with_context(|| format!("parsing folder-create response: {body_text}"))?;
    Ok(created.id)
}

// ==================== upload ====================

pub async fn upload_file(access_token: &str, parent_folder_id: &str, filename: &str, bytes: &[u8]) -> Result<UploadedFile> {
    upload_file_with(access_token, parent_folder_id, filename, bytes, &DriveEndpoints::default()).await
}

async fn upload_file_with(
    access_token: &str,
    parent_folder_id: &str,
    filename: &str,
    bytes: &[u8],
    endpoints: &DriveEndpoints,
) -> Result<UploadedFile> {
    #[derive(Deserialize)]
    struct UploadResponse {
        id: String,
        name: String,
    }

    // Real multipart upload per Drive's documented shape: a JSON metadata
    // part (name + parent folder) followed by a raw-bytes media part,
    // joined with one boundary. Not resumable - this app's payloads are
    // full snapshot bundles built up-front in memory already (see
    // ls-snapshot::bundle), so a single-request upload matches the rest of
    // this app's transport code rather than adding chunked-upload
    // bookkeeping nothing here needs yet.
    const BOUNDARY: &str = "localsync-cloud-drop-boundary";
    let metadata = serde_json::json!({ "name": filename, "parents": [parent_folder_id] }).to_string();
    let mut body = Vec::with_capacity(bytes.len() + metadata.len() + 256);
    body.extend_from_slice(format!("--{BOUNDARY}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n").as_bytes());
    body.extend_from_slice(format!("--{BOUNDARY}\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--").as_bytes());

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/files", endpoints.upload_base))
        .bearer_auth(access_token)
        .query(&[("uploadType", "multipart"), ("fields", "id,name")])
        .header("Content-Type", format!("multipart/related; boundary={BOUNDARY}"))
        .body(body)
        .send()
        .await
        .context("uploading file to Drive")?;
    let status = resp.status();
    let body_text = resp.text().await.context("reading upload response")?;
    if !status.is_success() {
        return Err(drive_error(status, body_text, "uploading file"));
    }
    let parsed: UploadResponse =
        serde_json::from_str(&body_text).with_context(|| format!("parsing upload response: {body_text}"))?;
    Ok(UploadedFile { file_id: parsed.id, name: parsed.name })
}

// ==================== permission grant ====================

pub async fn grant_reader_access(
    access_token: &str,
    file_id: &str,
    email: &str,
    expiration: Option<time::OffsetDateTime>,
) -> Result<GrantResult> {
    grant_reader_access_with(access_token, file_id, email, expiration, &DriveEndpoints::default()).await
}

async fn grant_reader_access_with(
    access_token: &str,
    file_id: &str,
    email: &str,
    expiration: Option<time::OffsetDateTime>,
    endpoints: &DriveEndpoints,
) -> Result<GrantResult> {
    #[derive(Deserialize)]
    struct PermissionResponse {
        id: String,
        #[serde(default, rename = "expirationTime")]
        expiration_time: Option<String>,
    }

    let mut body = serde_json::json!({
        "role": "reader",
        "type": "user",
        "emailAddress": email,
    });
    if let Some(exp) = expiration {
        let formatted = exp
            .format(&time::format_description::well_known::Rfc3339)
            .context("formatting expirationTime as RFC3339")?;
        // Always attempted (free real enforcement on Workspace accounts);
        // never trusted (see crate root doc comment) - `expiration_applied`
        // below is what actually tells the caller which case it got.
        body["expirationTime"] = serde_json::json!(formatted);
    }

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/files/{file_id}/permissions", endpoints.api_base))
        .bearer_auth(access_token)
        .query(&[("fields", "id,expirationTime")])
        .json(&body)
        .send()
        .await
        .context("granting Drive reader permission")?;
    let status = resp.status();
    let body_text = resp.text().await.context("reading permission-grant response")?;
    if !status.is_success() {
        return Err(drive_error(status, body_text, "granting permission"));
    }
    let parsed: PermissionResponse =
        serde_json::from_str(&body_text).with_context(|| format!("parsing permission-grant response: {body_text}"))?;

    Ok(GrantResult {
        permission_id: parsed.id,
        expiration_requested: expiration,
        expiration_applied: expiration.is_some() && parsed.expiration_time.is_some(),
    })
}

// ==================== download / delete / list ====================

pub async fn download_file(access_token: &str, file_id: &str) -> Result<Vec<u8>> {
    download_file_with(access_token, file_id, &DriveEndpoints::default()).await
}

async fn download_file_with(access_token: &str, file_id: &str, endpoints: &DriveEndpoints) -> Result<Vec<u8>> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/files/{file_id}", endpoints.api_base))
        .bearer_auth(access_token)
        .query(&[("alt", "media")])
        .send()
        .await
        .context("downloading file from Drive")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(drive_error(status, body, "downloading file"));
    }
    let bytes = resp.bytes().await.context("reading downloaded file bytes")?;
    Ok(bytes.to_vec())
}

pub async fn delete_file(access_token: &str, file_id: &str) -> Result<()> {
    delete_file_with(access_token, file_id, &DriveEndpoints::default()).await
}

async fn delete_file_with(access_token: &str, file_id: &str, endpoints: &DriveEndpoints) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = client
        .delete(format!("{}/files/{file_id}", endpoints.api_base))
        .bearer_auth(access_token)
        .send()
        .await
        .context("deleting file from Drive")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(drive_error(status, body, "deleting file"));
    }
    Ok(())
}

pub async fn list_permissions(access_token: &str, file_id: &str) -> Result<Vec<String>> {
    list_permissions_with(access_token, file_id, &DriveEndpoints::default()).await
}

async fn list_permissions_with(access_token: &str, file_id: &str, endpoints: &DriveEndpoints) -> Result<Vec<String>> {
    #[derive(Deserialize)]
    struct PermissionRef {
        #[serde(default, rename = "emailAddress")]
        email_address: Option<String>,
    }
    #[derive(Deserialize)]
    struct ListResponse {
        permissions: Vec<PermissionRef>,
    }

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/files/{file_id}/permissions", endpoints.api_base))
        .bearer_auth(access_token)
        .query(&[("fields", "permissions(id,emailAddress)")])
        .send()
        .await
        .context("listing Drive permissions")?;
    let status = resp.status();
    let body_text = resp.text().await.context("reading permissions-list response")?;
    if !status.is_success() {
        return Err(drive_error(status, body_text, "listing permissions"));
    }
    let parsed: ListResponse =
        serde_json::from_str(&body_text).with_context(|| format!("parsing permissions-list response: {body_text}"))?;
    Ok(parsed.permissions.into_iter().filter_map(|p| p.email_address).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn endpoints_for(server: &MockServer) -> DriveEndpoints {
        DriveEndpoints { api_base: server.uri(), upload_base: server.uri() }
    }

    // ---------- app folder ----------

    #[tokio::test]
    async fn finds_existing_app_folder_without_creating_a_new_one() {
        let server = MockServer::start().await;
        // No POST mock mounted at all: if the code wrongly tried to create
        // a folder too, that request would hit wiremock's unmatched-request
        // default (a 404), and `get_or_create_app_folder_with` would return
        // that as an error instead of the id below - so this test fails
        // loudly if the "already exists" short-circuit ever regresses.
        Mock::given(method("GET"))
            .and(path("/files"))
            .and(query_param("spaces", "drive"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "files": [{"id": "existing-folder-id", "name": "LocalSync Cloud Drop"}]
            })))
            .mount(&server)
            .await;

        let id = get_or_create_app_folder_with("token", &endpoints_for(&server).await).await.unwrap();
        assert_eq!(id, "existing-folder-id");
    }

    #[tokio::test]
    async fn creates_app_folder_when_none_exists() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "files": [] })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/files"))
            .and(body_string_contains("LocalSync Cloud Drop"))
            .and(body_string_contains("application/vnd.google-apps.folder"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "new-folder-id", "name": "LocalSync Cloud Drop"
            })))
            .mount(&server)
            .await;

        let id = get_or_create_app_folder_with("token", &endpoints_for(&server).await).await.unwrap();
        assert_eq!(id, "new-folder-id");
    }

    // ---------- upload ----------

    #[tokio::test]
    async fn upload_sends_expected_multipart_request_and_parses_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/files"))
            .and(query_param("uploadType", "multipart"))
            .and(wiremock::matchers::header_regex("Content-Type", "^multipart/related"))
            .and(body_string_contains("\"name\":\"snapshot.tar.gz\""))
            .and(body_string_contains("\"parents\":[\"folder-123\"]"))
            .and(body_string_contains("hello cloud drop"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "uploaded-file-id", "name": "snapshot.tar.gz"
            })))
            .mount(&server)
            .await;

        let uploaded = upload_file_with(
            "token",
            "folder-123",
            "snapshot.tar.gz",
            b"hello cloud drop",
            &endpoints_for(&server).await,
        )
        .await
        .unwrap();
        assert_eq!(uploaded.file_id, "uploaded-file-id");
        assert_eq!(uploaded.name, "snapshot.tar.gz");
    }

    // ---------- permission grant: the expiration_applied honesty check ----------

    #[tokio::test]
    async fn workspace_style_response_echoes_expiration_and_reports_applied_true() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/files/file-1/permissions"))
            .and(body_string_contains("expirationTime"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "perm-1",
                "type": "user",
                "role": "reader",
                "emailAddress": "receiver@company-workspace.com",
                "expirationTime": "2026-09-20T00:00:00.000Z"
            })))
            .mount(&server)
            .await;

        let requested = time::macros::datetime!(2026-09-20 00:00:00 UTC);
        let result = grant_reader_access_with(
            "token",
            "file-1",
            "receiver@company-workspace.com",
            Some(requested),
            &endpoints_for(&server).await,
        )
        .await
        .unwrap();

        assert_eq!(result.permission_id, "perm-1");
        assert_eq!(result.expiration_requested, Some(requested));
        assert!(result.expiration_applied, "Workspace-style response echoed expirationTime - must report applied=true");
    }

    #[tokio::test]
    async fn personal_account_style_response_omits_expiration_and_reports_applied_false() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/files/file-1/permissions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "perm-2",
                "type": "user",
                "role": "reader",
                "emailAddress": "receiver@gmail.com"
                // No expirationTime - the real, documented personal-account behavior.
            })))
            .mount(&server)
            .await;

        let requested = time::macros::datetime!(2026-09-20 00:00:00 UTC);
        let result = grant_reader_access_with(
            "token",
            "file-1",
            "receiver@gmail.com",
            Some(requested),
            &endpoints_for(&server).await,
        )
        .await
        .unwrap();

        assert_eq!(result.expiration_requested, Some(requested));
        assert!(!result.expiration_applied, "personal-account response silently dropped expirationTime - must report applied=false, not claim success");
    }

    #[tokio::test]
    async fn no_expiration_requested_is_never_reported_as_applied() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/files/file-1/permissions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "perm-3", "type": "user", "role": "reader", "emailAddress": "receiver@gmail.com"
            })))
            .mount(&server)
            .await;

        let result = grant_reader_access_with("token", "file-1", "receiver@gmail.com", None, &endpoints_for(&server).await)
            .await
            .unwrap();
        assert_eq!(result.expiration_requested, None);
        assert!(!result.expiration_applied);
    }

    #[tokio::test]
    async fn grant_403_insufficient_scope_surfaces_a_useful_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/files/file-1/permissions"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": {
                    "code": 403,
                    "message": "The user does not have sufficient permissions for this file.",
                    "errors": [{"message": "Insufficient Permission", "domain": "global", "reason": "insufficientFilePermissions"}]
                }
            })))
            .mount(&server)
            .await;

        let err = grant_reader_access_with("token", "file-1", "receiver@gmail.com", None, &endpoints_for(&server).await)
            .await
            .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("403"));
        assert!(message.contains("sufficient permissions"));
    }

    // ---------- download / delete / list ----------

    #[tokio::test]
    async fn download_returns_raw_bytes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/file-1"))
            .and(query_param("alt", "media"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"the actual file bytes".to_vec()))
            .mount(&server)
            .await;

        let bytes = download_file_with("token", "file-1", &endpoints_for(&server).await).await.unwrap();
        assert_eq!(bytes, b"the actual file bytes");
    }

    #[tokio::test]
    async fn download_404_surfaces_a_useful_error_not_a_panic() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/missing-file"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": {"code": 404, "message": "File not found: missing-file."}
            })))
            .mount(&server)
            .await;

        let err = download_file_with("token", "missing-file", &endpoints_for(&server).await).await.unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("404"));
        assert!(message.contains("File not found"));
    }

    #[tokio::test]
    async fn delete_succeeds_on_204() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/files/file-1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        delete_file_with("token", "file-1", &endpoints_for(&server).await).await.unwrap();
    }

    #[tokio::test]
    async fn delete_401_surfaces_a_useful_error() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/files/file-1"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {"code": 401, "message": "Invalid Credentials"}
            })))
            .mount(&server)
            .await;
        let err = delete_file_with("token", "file-1", &endpoints_for(&server).await).await.unwrap_err();
        assert!(format!("{err:#}").contains("401"));
    }

    #[tokio::test]
    async fn list_permissions_extracts_emails_and_skips_entries_without_one() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/file-1/permissions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "permissions": [
                    {"id": "owner-perm", "emailAddress": "owner@example.com"},
                    {"id": "anyone-perm"}
                ]
            })))
            .mount(&server)
            .await;

        let emails = list_permissions_with("token", "file-1", &endpoints_for(&server).await).await.unwrap();
        assert_eq!(emails, vec!["owner@example.com".to_string()]);
    }
}
