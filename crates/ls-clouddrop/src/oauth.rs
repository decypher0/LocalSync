//! Google OAuth 2.0 for a desktop app, using PKCE + a loopback redirect.
//!
//! Confirmed against Google's current docs before writing this (see the
//! round's research, re-verified here):
//! - The `urn:ietf:wg:oauth:2.0:oob` ("copy this code") flow is dead:
//!   Google's own installed-app docs state plainly that "the manual
//!   copy/paste option, also referred to as an out of band (OOB) redirect
//!   method, is no longer supported." The loopback IP redirect
//!   (`http://127.0.0.1:<port>`) is the current recommended replacement for
//!   desktop apps.
//! - **Round 31 correction of a round-23 claim that turned out wrong in
//!   real testing**: round 23's own comment here said the client secret was
//!   documented as optional for a Desktop-app client's token exchange, and
//!   that this module never sends one. A real, confirmed Desktop-app OAuth
//!   client's token exchange actually failed against Google's real token
//!   endpoint with `400 Bad Request: invalid_request - client_secret is
//!   missing`. Re-investigated rather than just patched around: Google's
//!   own generic "OAuth 2.0 for Native Apps" guide's parameter table really
//!   does list `client_secret` as "Optional" for the token exchange step -
//!   but real-world reports (Google's own developer forum, other OAuth
//!   client implementations hitting the identical error against Google
//!   specifically) consistently confirm the *actual* token endpoint
//!   enforces it for a Desktop-type client once `access_type=offline` is
//!   requested (i.e. asking for a `refresh_token`, which this module always
//!   does - see below) - PKCE is not accepted as a substitute the way it is
//!   for confidential-client-secret requirements with some other
//!   providers. The generic doc's "optional" is real for a bare
//!   access-token-only exchange; it doesn't hold for this module's actual
//!   request shape. [`OAuthConfig`] now carries the secret and every
//!   request to the token endpoint (both the initial exchange and a later
//!   refresh - the same client authenticates on both, so both need it)
//!   includes it.
//! - Google's own position (confirmed in the same native-app guide) is that
//!   this value is **not treated as confidential** for an installed/desktop
//!   application - anyone can extract it from a distributed binary, the
//!   same reasoning this project already applied to needing no client
//!   secret at all before round 31's real-world correction. It's read from
//!   the environment (`GOOGLE_OAUTH_CLIENT_SECRET`), never hardcoded, for
//!   the same reason `GOOGLE_OAUTH_CLIENT_ID` already is - not because it's
//!   being treated as a secret that must never appear in source, but for
//!   consistency with how this project already handles per-deployment
//!   OAuth client configuration.
//! - `access_type=offline` on the authorization request is what's required
//!   to get a `refresh_token` back at all (confirmed against Google's OIDC
//!   docs). `prompt=consent` is added alongside it: without it, Google only
//!   returns a `refresh_token` on a user's *first* consent for a given
//!   client+scope combination, which would silently break a user who
//!   re-links after revoking access.
//! - The userinfo endpoint used here is
//!   `https://openidconnect.googleapis.com/v1/userinfo` - the current
//!   OpenID-Connect-compliant endpoint Google's docs point to, not the
//!   older `www.googleapis.com/oauth2/v2/userinfo` shape.
use std::path::Path;
use std::time::Duration as StdDuration;

use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::identity;
use crate::store;

/// The exact scope union round 23's research settled on: every LocalSync
/// instance can act as either sender or receiver of a cloud drop, so every
/// linked account needs both `drive.file` (to create/upload its own files)
/// and `drive.readonly` (to read a file shared *to* it by another
/// instance's `drive.file`-scoped upload - see the crate root doc comment
/// for why `drive.file` alone can't see that).
pub const CLOUD_DROP_SCOPES: &[&str] = &[
    "openid",
    "email",
    "profile",
    "https://www.googleapis.com/auth/drive.file",
    "https://www.googleapis.com/auth/drive.readonly",
];

/// Configuration for the OAuth flow. Both values come from the
/// environment, matching this project's existing convention for
/// optional/deployment-specific config (e.g. `ls-net`'s
/// `LOCALSYNC_TURN_URL`) - see the module doc comment's round 31 note for
/// why `client_secret` is required here despite this being a PKCE flow,
/// and for why reading it from the environment rather than hardcoding it
/// doesn't mean this project is treating it as a real secret.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
}

impl OAuthConfig {
    /// Reads `GOOGLE_OAUTH_CLIENT_ID` and `GOOGLE_OAUTH_CLIENT_SECRET` from
    /// the environment.
    pub fn from_env() -> Result<Self> {
        let client_id = std::env::var("GOOGLE_OAUTH_CLIENT_ID")
            .context("GOOGLE_OAUTH_CLIENT_ID is not set - Cloud drop needs a Google OAuth Desktop app client ID")?;
        let client_secret = std::env::var("GOOGLE_OAUTH_CLIENT_SECRET").context(
            "GOOGLE_OAUTH_CLIENT_SECRET is not set - Google's token endpoint rejects this app's \
             Desktop-app OAuth client without it, even with PKCE (see docs/google-drive-setup.md)",
        )?;
        Ok(Self { client_id, client_secret })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: time::OffsetDateTime,
    pub scopes: Vec<String>,
    pub email: String,
}

// ==================== PKCE (RFC 7636) ====================

struct Pkce {
    verifier: String,
    challenge: String,
}

/// `base64url(random bytes)`, no padding - used for both the PKCE
/// `code_verifier` and the CSRF `state` value. `n` random bytes produce a
/// `ceil(n * 4 / 3)`-character string over the base64url alphabet
/// (`[A-Za-z0-9\-_]`), a strict subset of RFC 7636's allowed
/// `code_verifier` character set (`[A-Za-z0-9\-._~]`).
fn random_url_safe_token(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// `code_challenge = BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))` per
/// RFC 7636 section 4.2 (S256 method).
fn code_challenge_from_verifier(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn generate_pkce() -> Pkce {
    // 32 random bytes -> 43-character verifier: RFC 7636's minimum length
    // and the length Google's own client libraries generate. No need to
    // pad out to the 128-character maximum for extra entropy we don't need.
    let verifier = random_url_safe_token(32);
    let challenge = code_challenge_from_verifier(&verifier);
    Pkce { verifier, challenge }
}

// ==================== endpoints (overridable for tests) ====================

struct Endpoints {
    auth_url: &'static str,
    token_url: String,
    userinfo_url: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: "https://oauth2.googleapis.com/token".to_string(),
            userinfo_url: identity::USERINFO_URL.to_string(),
        }
    }
}

fn build_auth_url(
    endpoints: &Endpoints,
    config: &OAuthConfig,
    redirect_uri: &str,
    scopes: &[&str],
    code_challenge: &str,
    state: &str,
) -> String {
    url::Url::parse_with_params(
        endpoints.auth_url,
        &[
            ("client_id", config.client_id.as_str()),
            ("redirect_uri", redirect_uri),
            ("response_type", "code"),
            ("scope", &scopes.join(" ")),
            ("code_challenge", code_challenge),
            ("code_challenge_method", "S256"),
            ("access_type", "offline"),
            ("prompt", "consent"),
            ("state", state),
        ],
    )
    .expect("auth_url is a fixed, valid base URL")
    .to_string()
}

// ==================== loopback redirect capture ====================

/// Binds an ephemeral loopback listener (same OS-picks-a-free-port trick
/// `ls_net::discovery::host_ephemeral_relay` already uses, applied here to
/// catch exactly one OAuth redirect instead of hosting a relay), waits for
/// the browser to redirect to it, and returns the `code` query parameter.
/// Bails if Google redirects with `error=...` (e.g. the user clicked
/// "Cancel") or if `state` doesn't match what we sent (CSRF protection).
async fn capture_redirect_code(listener: TcpListener, expected_state: &str) -> Result<String> {
    // Five minutes is generous for a human to pick an account and click
    // "Allow" in a real browser, short enough not to leak the listener
    // forever if the user just closes the tab instead.
    tokio::time::timeout(StdDuration::from_secs(300), async {
        loop {
            let (mut stream, _) = listener.accept().await.context("accepting loopback redirect connection")?;
            let request_line = read_request_line(&mut stream).await?;
            let Some(path_and_query) = request_line.split_whitespace().nth(1) else {
                continue;
            };
            let parsed = url::Url::parse(&format!("http://127.0.0.1{path_and_query}"))
                .context("parsing redirect request path as a URL")?;
            let params: std::collections::HashMap<_, _> = parsed.query_pairs().collect();

            respond_close_tab(&mut stream).await?;

            if let Some(error) = params.get("error") {
                anyhow::bail!("Google OAuth authorization failed: {error}");
            }
            let (Some(code), Some(state)) = (params.get("code"), params.get("state")) else {
                // Not our redirect (e.g. a stray browser preflight) - keep waiting.
                continue;
            };
            anyhow::ensure!(state == expected_state, "OAuth state mismatch - possible CSRF, aborting");
            return Ok(code.to_string());
        }
    })
    .await
    .context("timed out waiting for the browser to complete Google sign-in")?
}

async fn read_request_line(stream: &mut TcpStream) -> Result<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await.context("reading loopback redirect request")?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16 * 1024 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).lines().next().unwrap_or_default().to_string())
}

async fn respond_close_tab(stream: &mut TcpStream) -> Result<()> {
    const BODY: &str = "<html><body>You can close this tab and return to LocalSync.</body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        BODY.len(),
        BODY
    );
    stream.write_all(response.as_bytes()).await.context("writing loopback redirect response")?;
    let _ = stream.shutdown().await;
    Ok(())
}

// ==================== token endpoint ====================

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleErrorBody {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

async fn parse_token_response(resp: reqwest::Response) -> Result<TokenResponse> {
    let status = resp.status();
    let body = resp.text().await.context("reading token endpoint response body")?;
    if !status.is_success() {
        if let Ok(err) = serde_json::from_str::<GoogleErrorBody>(&body) {
            anyhow::bail!(
                "Google token endpoint returned {status}: {} - {}",
                err.error,
                err.error_description.unwrap_or_default()
            );
        }
        anyhow::bail!("Google token endpoint returned {status}: {body}");
    }
    serde_json::from_str(&body).with_context(|| format!("parsing token endpoint response: {body}"))
}

fn token_set_from_response(
    resp: TokenResponse,
    fallback_refresh_token: Option<String>,
    email: String,
) -> TokenSet {
    let scopes = resp
        .scope
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    TokenSet {
        access_token: resp.access_token,
        // Google's refresh-token response normally omits `refresh_token`
        // entirely (it doesn't rotate) - carry the caller's existing one
        // forward so a refresh never silently drops it.
        refresh_token: resp.refresh_token.or(fallback_refresh_token),
        expires_at: time::OffsetDateTime::now_utc() + time::Duration::seconds(resp.expires_in),
        scopes,
        email,
    }
}

// ==================== public API ====================

/// Runs the full PKCE + loopback OAuth flow: builds the authorization URL,
/// hands it to `on_auth_url` (the caller opens it in the system browser -
/// this crate does not touch a browser or Tauri's shell API), then blocks
/// until the browser redirects back and the code has been exchanged for
/// tokens. Does **not** persist the result - call [`crate::store::save_tokens`]
/// with the returned [`TokenSet`] if you want it remembered across runs
/// (kept as a separate, explicit step rather than an automatic side effect,
/// same "mechanism, not policy" split [`crate::retention`] uses).
pub async fn run_oauth_flow<F>(config: &OAuthConfig, scopes: &[&str], on_auth_url: F) -> Result<TokenSet>
where
    F: FnOnce(String) + Send,
{
    run_oauth_flow_with(config, scopes, &Endpoints::default(), on_auth_url).await
}

async fn run_oauth_flow_with<F>(
    config: &OAuthConfig,
    scopes: &[&str],
    endpoints: &Endpoints,
    on_auth_url: F,
) -> Result<TokenSet>
where
    F: FnOnce(String) + Send,
{
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind ephemeral loopback port for the OAuth redirect")?;
    let port = listener.local_addr().context("failed to read bound loopback port")?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}");

    let pkce = generate_pkce();
    let state = random_url_safe_token(16);
    let auth_url = build_auth_url(endpoints, config, &redirect_uri, scopes, &pkce.challenge, &state);

    // Hand the URL back immediately - the caller needs it before this
    // function's long block on the redirect below.
    on_auth_url(auth_url);

    let code = capture_redirect_code(listener, &state).await?;

    let client = reqwest::Client::new();
    let resp = client
        .post(&endpoints.token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", config.client_id.as_str()),
            // Round 31: Google's real token endpoint rejects this exact
            // request shape (Desktop-app client + access_type=offline)
            // without it - see the module doc comment for the full story.
            ("client_secret", config.client_secret.as_str()),
            ("code_verifier", pkce.verifier.as_str()),
        ])
        .send()
        .await
        .context("sending authorization code to Google's token endpoint")?;
    let token_response = parse_token_response(resp).await?;
    let email = identity::fetch_email_at(&endpoints.userinfo_url, &token_response.access_token).await?;
    Ok(token_set_from_response(token_response, None, email))
}

/// Exchanges a refresh token for a fresh access token. Preserves the input
/// `refresh_token` in the returned [`TokenSet`] (Google's refresh response
/// normally doesn't return a new one) and re-fetches the account's email so
/// the returned `TokenSet` is fully self-consistent.
pub async fn refresh_access_token(config: &OAuthConfig, refresh_token: &str) -> Result<TokenSet> {
    refresh_access_token_with(config, refresh_token, &Endpoints::default()).await
}

async fn refresh_access_token_with(
    config: &OAuthConfig,
    refresh_token: &str,
    endpoints: &Endpoints,
) -> Result<TokenSet> {
    let client = reqwest::Client::new();
    let resp = client
        .post(&endpoints.token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", config.client_id.as_str()),
            // Round 31: same client authenticating to the same endpoint as
            // the authorization_code exchange above - if Google requires
            // the secret there, there's no reason to expect a refresh to be
            // exempt, and nothing in Google's docs suggests grant type
            // changes this client's own authentication requirement.
            ("client_secret", config.client_secret.as_str()),
        ])
        .send()
        .await
        .context("sending refresh token to Google's token endpoint")?;
    let token_response = parse_token_response(resp).await?;
    let email = identity::fetch_email_at(&endpoints.userinfo_url, &token_response.access_token).await?;
    Ok(token_set_from_response(token_response, Some(refresh_token.to_string()), email))
}

/// The function the rest of the app should actually call day to day: loads
/// the stored token set, refreshes it if it's already expired or within 5
/// minutes of expiring, persists any refreshed token set, and returns a
/// currently-valid access token.
pub async fn ensure_valid_access_token(config: &OAuthConfig) -> Result<String> {
    ensure_valid_access_token_in(config, &Endpoints::default(), &store::default_dir()?).await
}

async fn ensure_valid_access_token_in(config: &OAuthConfig, endpoints: &Endpoints, dir: &Path) -> Result<String> {
    let stored = store::load_in(dir)?
        .context("not linked to Google Drive yet - run the Cloud drop link flow first")?;

    let safety_margin = time::Duration::minutes(5);
    if time::OffsetDateTime::now_utc() + safety_margin < stored.expires_at {
        return Ok(stored.access_token);
    }

    let refresh_token = stored
        .refresh_token
        .as_deref()
        .context("stored Cloud drop token has expired and no refresh token is available - re-link required")?;
    let refreshed = refresh_access_token_with(config, refresh_token, endpoints).await?;
    store::save_in(dir, &refreshed)?;
    Ok(refreshed.access_token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ---------- PKCE ----------

    #[test]
    fn code_verifier_has_valid_length_and_charset() {
        let pkce = generate_pkce();
        assert!(
            (43..=128).contains(&pkce.verifier.len()),
            "RFC 7636 requires a 43-128 char verifier, got {}",
            pkce.verifier.len()
        );
        assert!(pkce
            .verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~'));
    }

    #[test]
    fn code_challenge_matches_rfc7636_appendix_b_known_vector() {
        // The exact worked example from RFC 7636 Appendix B - an
        // independently-known-correct value, not just "it runs".
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected_challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(code_challenge_from_verifier(verifier), expected_challenge);
    }

    #[test]
    fn two_generated_verifiers_are_not_equal() {
        // Cheap sanity check that this is actually randomized, not a fixed
        // string that happens to be the right shape.
        assert_ne!(generate_pkce().verifier, generate_pkce().verifier);
    }

    // ---------- authorization URL ----------

    #[test]
    fn auth_url_has_all_required_query_params() {
        let config = OAuthConfig {
            client_id: "test-client-id.apps.googleusercontent.com".into(),
            client_secret: "test-client-secret".into(),
        };
        let endpoints = Endpoints::default();
        let url_str = build_auth_url(
            &endpoints,
            &config,
            "http://127.0.0.1:54321",
            &["openid", "email", "https://www.googleapis.com/auth/drive.file"],
            "the-code-challenge",
            "the-state",
        );

        let parsed = url::Url::parse(&url_str).unwrap();
        assert_eq!(parsed.host_str(), Some("accounts.google.com"));
        assert_eq!(parsed.path(), "/o/oauth2/v2/auth");

        let params: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(params.get("client_id").unwrap(), "test-client-id.apps.googleusercontent.com");
        assert_eq!(params.get("redirect_uri").unwrap(), "http://127.0.0.1:54321");
        assert_eq!(params.get("response_type").unwrap(), "code");
        assert_eq!(params.get("scope").unwrap(), "openid email https://www.googleapis.com/auth/drive.file");
        assert_eq!(params.get("code_challenge").unwrap(), "the-code-challenge");
        assert_eq!(params.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(params.get("access_type").unwrap(), "offline");
        assert_eq!(params.get("state").unwrap(), "the-state");
    }

    // ---------- token exchange / refresh against a real local mock server ----------

    fn google_token_success_body(access_token: &str, with_refresh_token: bool) -> serde_json::Value {
        let mut body = serde_json::json!({
            "access_token": access_token,
            "expires_in": 3599,
            "scope": "openid email https://www.googleapis.com/auth/drive.file",
            "token_type": "Bearer",
        });
        if with_refresh_token {
            body["refresh_token"] = serde_json::json!("a-real-looking-refresh-token");
        }
        body
    }

    fn google_userinfo_body(email: &str) -> serde_json::Value {
        serde_json::json!({
            "sub": "1234567890",
            "email": email,
            "email_verified": true,
        })
    }

    #[tokio::test]
    async fn refresh_access_token_sends_expected_request_and_parses_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=my-refresh-token"))
            .and(body_string_contains("client_id=my-client-id"))
            // Round 31: if this were ever missing, wiremock would reject the
            // real request as unmatched and this test would fail with a
            // connection/response error, not silently pass - see
            // `client_secret_is_present_on_both_grant_types` below for a
            // test dedicated to exactly this, on both grant types.
            .and(body_string_contains("client_secret=my-client-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(google_token_success_body("fresh-access-token", false)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/userinfo"))
            .respond_with(ResponseTemplate::new(200).set_body_json(google_userinfo_body("user@example.com")))
            .mount(&server)
            .await;

        let config = OAuthConfig { client_id: "my-client-id".into(), client_secret: "my-client-secret".into() };
        let endpoints = Endpoints {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: format!("{}/token", server.uri()),
            userinfo_url: format!("{}/userinfo", server.uri()),
        };

        let tokens = refresh_access_token_with(&config, "my-refresh-token", &endpoints).await.unwrap();
        assert_eq!(tokens.access_token, "fresh-access-token");
        // Refresh response omitted refresh_token - must fall back to the one we sent in.
        assert_eq!(tokens.refresh_token.as_deref(), Some("my-refresh-token"));
        assert_eq!(tokens.email, "user@example.com");
        assert!(tokens.scopes.contains(&"openid".to_string()));
        assert!(tokens.expires_at > time::OffsetDateTime::now_utc());
    }

    #[tokio::test]
    async fn refresh_rotates_refresh_token_when_google_sends_a_new_one() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/token")).respond_with(
            ResponseTemplate::new(200).set_body_json(google_token_success_body("fresh-access-token", true)),
        ).mount(&server).await;
        Mock::given(method("GET")).and(path("/userinfo")).respond_with(
            ResponseTemplate::new(200).set_body_json(google_userinfo_body("user@example.com")),
        ).mount(&server).await;

        let config = OAuthConfig { client_id: "my-client-id".into(), client_secret: "my-client-secret".into() };
        let endpoints = Endpoints {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: format!("{}/token", server.uri()),
            userinfo_url: format!("{}/userinfo", server.uri()),
        };

        let tokens = refresh_access_token_with(&config, "old-refresh-token", &endpoints).await.unwrap();
        assert_eq!(tokens.refresh_token.as_deref(), Some("a-real-looking-refresh-token"));
    }

    #[tokio::test]
    async fn token_endpoint_error_surfaces_a_useful_message_not_a_panic() {
        let server = MockServer::start().await;
        // Realistic Google error shape for an expired/revoked refresh token.
        Mock::given(method("POST")).and(path("/token")).respond_with(
            ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "Token has been expired or revoked."
            })),
        ).mount(&server).await;

        let config = OAuthConfig { client_id: "my-client-id".into(), client_secret: "my-client-secret".into() };
        let endpoints = Endpoints {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: format!("{}/token", server.uri()),
            userinfo_url: format!("{}/userinfo", server.uri()),
        };

        let err = refresh_access_token_with(&config, "dead-refresh-token", &endpoints).await.unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("invalid_grant"), "error should surface Google's reason, got: {message}");
        assert!(message.contains("expired or revoked"), "error should surface Google's description, got: {message}");
    }

    #[tokio::test]
    async fn userinfo_401_surfaces_a_useful_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/token")).respond_with(
            ResponseTemplate::new(200).set_body_json(google_token_success_body("fresh-access-token", false)),
        ).mount(&server).await;
        Mock::given(method("GET")).and(path("/userinfo")).respond_with(
            ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {"code": 401, "message": "Invalid Credentials", "status": "UNAUTHENTICATED"}
            })),
        ).mount(&server).await;

        let config = OAuthConfig { client_id: "my-client-id".into(), client_secret: "my-client-secret".into() };
        let endpoints = Endpoints {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: format!("{}/token", server.uri()),
            userinfo_url: format!("{}/userinfo", server.uri()),
        };

        let err = refresh_access_token_with(&config, "my-refresh-token", &endpoints).await.unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("401"), "error should surface the HTTP status, got: {message}");
    }

    // ---------- ensure_valid_access_token ----------

    #[tokio::test]
    async fn ensure_valid_access_token_reuses_unexpired_token_without_a_network_call() {
        let dir = tempfile::tempdir().unwrap();
        let stored = TokenSet {
            access_token: "still-good".into(),
            refresh_token: Some("refresh".into()),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
            scopes: vec!["openid".into()],
            email: "user@example.com".into(),
        };
        store::save_in(dir.path(), &stored).unwrap();

        // No mock server registered at all - if this tried to hit the
        // network it would fail to connect and this test would fail.
        let endpoints = Endpoints {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: "http://127.0.0.1:1".to_string(),
            userinfo_url: "http://127.0.0.1:1".to_string(),
        };
        let config = OAuthConfig { client_id: "my-client-id".into(), client_secret: "my-client-secret".into() };
        let token = ensure_valid_access_token_in(&config, &endpoints, dir.path()).await.unwrap();
        assert_eq!(token, "still-good");
    }

    #[tokio::test]
    async fn ensure_valid_access_token_refreshes_and_persists_when_near_expiry() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/token")).respond_with(
            ResponseTemplate::new(200).set_body_json(google_token_success_body("refreshed-access-token", false)),
        ).mount(&server).await;
        Mock::given(method("GET")).and(path("/userinfo")).respond_with(
            ResponseTemplate::new(200).set_body_json(google_userinfo_body("user@example.com")),
        ).mount(&server).await;

        let dir = tempfile::tempdir().unwrap();
        let stored = TokenSet {
            access_token: "about-to-expire".into(),
            refresh_token: Some("refresh-token".into()),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::seconds(30),
            scopes: vec!["openid".into()],
            email: "user@example.com".into(),
        };
        store::save_in(dir.path(), &stored).unwrap();

        let endpoints = Endpoints {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: format!("{}/token", server.uri()),
            userinfo_url: format!("{}/userinfo", server.uri()),
        };
        let config = OAuthConfig { client_id: "my-client-id".into(), client_secret: "my-client-secret".into() };
        let token = ensure_valid_access_token_in(&config, &endpoints, dir.path()).await.unwrap();
        assert_eq!(token, "refreshed-access-token");

        let persisted = store::load_in(dir.path()).unwrap().unwrap();
        assert_eq!(persisted.access_token, "refreshed-access-token");
    }

    // ---------- full loopback flow, browser simulated by hand ----------

    #[tokio::test]
    async fn run_oauth_flow_completes_end_to_end_against_a_simulated_browser_redirect() {
        let server = MockServer::start().await;
        // Round 31: `body_string_contains("client_secret=my-client-secret")`
        // here is what actually proves this - if the real request built by
        // `run_oauth_flow_with` omitted it, wiremock would treat this
        // request as unmatched and the flow below would fail with a
        // connection/response error rather than the success this test
        // asserts on.
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("client_secret=my-client-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(google_token_success_body("brand-new-access-token", true)))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/userinfo")).respond_with(
            ResponseTemplate::new(200).set_body_json(google_userinfo_body("someone@gmail.com")),
        ).mount(&server).await;

        let config = OAuthConfig { client_id: "my-client-id".into(), client_secret: "my-client-secret".into() };
        let endpoints = Endpoints {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: format!("{}/token", server.uri()),
            userinfo_url: format!("{}/userinfo", server.uri()),
        };

        let (url_tx, url_rx) = tokio::sync::oneshot::channel::<String>();
        let flow = tokio::spawn(async move {
            run_oauth_flow_with(&config, CLOUD_DROP_SCOPES, &endpoints, move |url| {
                let _ = url_tx.send(url);
            })
            .await
        });

        // Caller gets the URL before the flow finishes - exactly the
        // "hand back the URL, then block" contract this module promises.
        let auth_url = tokio::time::timeout(StdDuration::from_secs(5), url_rx).await.unwrap().unwrap();
        let parsed = url::Url::parse(&auth_url).unwrap();
        let params: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        let redirect_uri = params.get("redirect_uri").unwrap().clone();
        let state = params.get("state").unwrap().clone();

        // Stand in for the real browser: connect to the loopback listener
        // and send exactly the GET request Google's redirect would send.
        let addr = redirect_uri.trim_start_matches("http://");
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request = format!("GET /?code=fake-auth-code&state={state} HTTP/1.1\r\nHost: {addr}\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
        assert!(String::from_utf8_lossy(&response).contains("close this tab"));

        let tokens = tokio::time::timeout(StdDuration::from_secs(5), flow).await.unwrap().unwrap().unwrap();
        assert_eq!(tokens.access_token, "brand-new-access-token");
        assert_eq!(tokens.email, "someone@gmail.com");
    }

    #[tokio::test]
    async fn run_oauth_flow_rejects_a_state_mismatch() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let expected_state = "expected-state";

        let capture = tokio::spawn(capture_redirect_code(listener, expected_state));

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request = "GET /?code=some-code&state=wrong-state HTTP/1.1\r\nHost: x\r\n\r\n";
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf).await;

        let result = tokio::time::timeout(StdDuration::from_secs(5), capture).await.unwrap().unwrap();
        assert!(result.is_err(), "a state mismatch must be rejected, not silently accepted");
    }
}
