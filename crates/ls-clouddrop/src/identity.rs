//! Resolves the signed-in Google account's email address from an access
//! token - needed so [`crate::drive::grant_reader_access`] has a concrete
//! `emailAddress` to target and so [`crate::oauth::TokenSet`] can record
//! which account a token belongs to.
//!
//! Uses `https://openidconnect.googleapis.com/v1/userinfo` - the current
//! OpenID-Connect-compliant userinfo endpoint Google's own OIDC docs point
//! to, not the older, differently-shaped
//! `https://www.googleapis.com/oauth2/v2/userinfo`.

use anyhow::{Context, Result};
use serde::Deserialize;

pub(crate) const USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";

#[derive(Debug, Deserialize)]
struct UserInfo {
    email: Option<String>,
}

pub async fn fetch_email(access_token: &str) -> Result<String> {
    fetch_email_at(USERINFO_URL, access_token).await
}

/// `pub(crate)` so `oauth.rs` can call it with a wiremock URL in tests and
/// re-use it internally after a real token exchange/refresh, without
/// exposing the base-URL override as part of this crate's public API.
pub(crate) async fn fetch_email_at(base_url: &str, access_token: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let resp = client
        .get(base_url)
        .bearer_auth(access_token)
        .send()
        .await
        .context("requesting userinfo from Google")?;

    let status = resp.status();
    let body = resp.text().await.context("reading userinfo response body")?;
    if !status.is_success() {
        anyhow::bail!("userinfo request failed with {status}: {body}");
    }

    let info: UserInfo =
        serde_json::from_str(&body).with_context(|| format!("parsing userinfo response: {body}"))?;
    info.email.context("userinfo response did not include an email field")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn parses_email_from_a_realistic_userinfo_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/userinfo"))
            .and(header("Authorization", "Bearer test-access-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sub": "111111111111111111111",
                "name": "Ada Lovelace",
                "given_name": "Ada",
                "family_name": "Lovelace",
                "picture": "https://lh3.googleusercontent.com/a/example",
                "email": "ada@example.com",
                "email_verified": true,
                "locale": "en"
            })))
            .mount(&server)
            .await;

        let email = fetch_email_at(&format!("{}/userinfo", server.uri()), "test-access-token")
            .await
            .unwrap();
        assert_eq!(email, "ada@example.com");
    }

    #[tokio::test]
    async fn expired_token_401_surfaces_a_useful_error_not_a_panic() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/userinfo"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": "invalid_token",
                "error_description": "Invalid Credentials"
            })))
            .mount(&server)
            .await;

        let err = fetch_email_at(&format!("{}/userinfo", server.uri()), "expired-token")
            .await
            .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("401"), "expected the status code in the error, got: {message}");
    }

    #[tokio::test]
    async fn response_missing_email_field_is_a_clear_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/userinfo"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sub": "111111111111111111111"
            })))
            .mount(&server)
            .await;

        let err = fetch_email_at(&format!("{}/userinfo", server.uri()), "token")
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("email"));
    }
}
