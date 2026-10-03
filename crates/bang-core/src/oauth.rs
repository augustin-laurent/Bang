//! OAuth2 (PKCE) flows for Gmail and Microsoft.
//!
//! The flow opens the system browser, listens for the redirect on a
//! localhost server, and exchanges the authorization code for tokens.

use std::time::Duration;

use oauth2::basic::{BasicClient, BasicTokenResponse};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, CsrfToken, PkceCodeChallenge, RedirectUrl, Scope,
    TokenResponse, TokenUrl,
};

use crate::account::OAuthProvider;
use crate::error::{Error, Result};

/// Tokens obtained from an OAuth2 flow.
#[derive(Debug, Clone)]
pub struct OAuthTokens {
    /// Access token.
    pub access_token: String,
    /// Refresh token, if the provider issued one.
    pub refresh_token: Option<String>,
    /// Expiry as Unix timestamp (seconds), if known.
    pub expires_at: Option<i64>,
}

/// Endpoints and scopes for a provider.
struct ProviderConfig {
    auth_url: &'static str,
    token_url: &'static str,
    scopes: &'static [&'static str],
}

fn provider_config(provider: OAuthProvider) -> ProviderConfig {
    match provider {
        OAuthProvider::Google => ProviderConfig {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: "https://oauth2.googleapis.com/token",
            scopes: &[
                "https://mail.google.com/",
                "https://www.googleapis.com/auth/userinfo.email",
            ],
        },
        OAuthProvider::Microsoft => ProviderConfig {
            auth_url: "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize",
            token_url: "https://login.microsoftonline.com/consumers/oauth2/v2.0/token",
            scopes: &[
                "IMAP.AccessAsUser.All",
                "offline_access",
                "https://outlook.office.com/SMTP.Send",
            ],
        },
    }
}

/// Start an interactive OAuth2 PKCE flow.
///
/// Opens the system browser, waits up to 5 minutes for the redirect on a
/// localhost server, and exchanges the code for tokens.
///
/// # Arguments
/// * `provider` — which provider to authenticate against.
/// * `client_id` — the OAuth2 client id registered for this app.
///
/// # Errors
/// Returns `Error::Auth` or `Error::Config` on failure.
pub async fn start_flow(provider: OAuthProvider, client_id: &str) -> Result<OAuthTokens> {
    let cfg = provider_config(provider);
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();

    // Local redirect server on a random port.
    let server = tiny_http::Server::http("127.0.0.1:0")
        .map_err(|e| Error::Auth(format!("redirect server: {e}")))?;
    let port = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| Error::Auth("redirect server not on an IP address".into()))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let client = BasicClient::new(ClientId::new(client_id.to_string()))
        .set_auth_uri(
            AuthUrl::new(cfg.auth_url.to_string())
                .map_err(|e| Error::Config(e.to_string()))?,
        )
        .set_token_uri(
            TokenUrl::new(cfg.token_url.to_string())
                .map_err(|e| Error::Config(e.to_string()))?,
        )
        .set_redirect_uri(
            RedirectUrl::new(redirect_uri)
                .map_err(|e| Error::Config(e.to_string()))?,
        );

    let mut request = client
        .authorize_url(CsrfToken::new_random)
        .set_pkce_challenge(challenge);
    for scope in cfg.scopes {
        request = request.add_scope(Scope::new(scope.to_string()));
    }
    let (auth_url, state) = request.url();

    webbrowser::open(auth_url.as_str())
        .map_err(|e| Error::Auth(format!("open browser: {e}")))?;

    // Wait for the redirect (blocking, off the async runtime).
    let code = tokio::task::spawn_blocking(move || wait_for_code(&server, &state))
        .await
        .map_err(|e| Error::Auth(e.to_string()))?
        .map_err(Error::Auth)?;

    let http = reqwest::Client::new();
    let tokens: BasicTokenResponse = client
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(verifier)
        .request_async(&http)
        .await
        .map_err(|e| Error::Auth(format!("token exchange: {e}")))?;

    let expires_at = tokens.expires_in().map(|d| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|n| n.as_secs() as i64 + d.as_secs() as i64)
            .unwrap_or(0)
    });
    Ok(OAuthTokens {
        access_token: tokens.access_token().secret().to_string(),
        refresh_token: tokens
            .refresh_token()
            .map(|r| r.secret().to_string()),
        expires_at,
    })
}

/// Fetch the account's email address using a fresh access token.
///
/// # Errors
/// Returns `Error::Auth` on failure.
pub async fn fetch_email(provider: OAuthProvider, access_token: &str) -> Result<String> {
    let url = match provider {
        OAuthProvider::Google => "https://www.googleapis.com/oauth2/v2/userinfo",
        OAuthProvider::Microsoft => "https://graph.microsoft.com/v1.0/me",
    };
    let resp = reqwest::Client::new()
        .get(url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| Error::Auth(e.to_string()))?;
    let json: serde_json::Value =
        resp.json().await.map_err(|e| Error::Auth(e.to_string()))?;
    let email = json
        .get("email")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Auth("no email in userinfo response".into()))?;
    Ok(email.to_string())
}

/// Block until the browser redirects, then return the authorization code.
fn wait_for_code(
    server: &tiny_http::Server,
    expected_state: &CsrfToken,
) -> std::result::Result<String, String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    loop {
        if std::time::Instant::now() > deadline {
            return Err("timed out waiting for browser redirect".into());
        }
        let request = match server.recv_timeout(Duration::from_secs(1)) {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(e) => return Err(e.to_string()),
        };
        let url = url::Url::parse(&request.url()).map_err(|e| e.to_string())?;
        let params: std::collections::HashMap<String, String> =
            url.query_pairs().into_owned().collect();

        let body = match params.get("error") {
            Some(err) => format!("OAuth error: {err}"),
            None => "Bang received your sign-in. You can close this tab and return to Bang."
                .to_string(),
        };
        let response = tiny_http::Response::from_string(body).with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
                .unwrap(),
        );
        request.respond(response).map_err(|e| e.to_string())?;

        if let Some(code) = params.get("code") {
            let state = params.get("state").ok_or("missing state parameter")?;
            if state != expected_state.secret() {
                return Err("state mismatch (possible CSRF)".into());
            }
            return Ok(code.clone());
        }
    }
}
