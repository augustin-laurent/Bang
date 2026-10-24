//! OAuth2 (PKCE) flows for Gmail and Microsoft.
//!
//! The flow opens the system browser, listens for the redirect on a
//! localhost server, and exchanges the authorization code for tokens.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use oauth2::basic::{BasicClient, BasicTokenResponse};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge, RedirectUrl,
    Scope, TokenResponse, TokenUrl,
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
            auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            scopes: &[
                "https://outlook.office.com/IMAP.AccessAsUser.All",
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
    start_flow_with_options(provider, client_id, None, "").await
}

/// Start a desktop sign-in with an optional client secret and mailbox hint.
/// Dropping this future also cancels the loopback listener.
///
/// # Errors
/// Returns an authentication or configuration error.
pub async fn start_flow_with_options(
    provider: OAuthProvider,
    client_id: &str,
    client_secret: Option<&str>,
    email: &str,
) -> Result<OAuthTokens> {
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
    // Microsoft ignores ephemeral ports for registered localhost redirects.
    let host = if provider == OAuthProvider::Microsoft {
        "localhost"
    } else {
        "127.0.0.1"
    };
    let redirect_uri = format!("http://{host}:{port}/callback");

    let mut client = BasicClient::new(ClientId::new(client_id.to_string()))
        .set_auth_type(oauth2::AuthType::RequestBody)
        .set_auth_uri(
            AuthUrl::new(cfg.auth_url.to_string()).map_err(|e| Error::Config(e.to_string()))?,
        )
        .set_token_uri(
            TokenUrl::new(cfg.token_url.to_string()).map_err(|e| Error::Config(e.to_string()))?,
        )
        .set_redirect_uri(
            RedirectUrl::new(redirect_uri).map_err(|e| Error::Config(e.to_string()))?,
        );

    if let Some(secret) = client_secret.filter(|s| !s.is_empty()) {
        client = client.set_client_secret(ClientSecret::new(secret.to_owned()));
    }
    let mut request = client
        .authorize_url(CsrfToken::new_random)
        .set_pkce_challenge(challenge);
    for scope in cfg.scopes {
        request = request.add_scope(Scope::new(scope.to_string()));
    }
    if !email.is_empty() {
        request = request.add_extra_param("login_hint", email);
    }
    if provider == OAuthProvider::Google {
        request = request
            .add_extra_param("access_type", "offline")
            .add_extra_param("prompt", "consent");
    } else {
        request = request.add_extra_param("prompt", "select_account");
    }
    let (auth_url, state) = request.url();

    webbrowser::open(auth_url.as_str()).map_err(|e| Error::Auth(format!("open browser: {e}")))?;

    // Wait for the redirect (blocking, off the async runtime).
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel_on_drop = CancelListener(cancelled.clone());
    let code = tokio::task::spawn_blocking(move || wait_for_code(&server, &state, &cancelled))
        .await
        .map_err(|e| Error::Auth(e.to_string()))?
        .map_err(Error::Auth)?;

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| Error::Network(e.to_string()))?;
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
        refresh_token: tokens.refresh_token().map(|r| r.secret().to_string()),
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
        OAuthProvider::Microsoft => return Err(Error::Config(
            "Microsoft sign-in requires the mailbox address; Exchange tokens cannot call Graph."
                .into(),
        )),
    };
    let resp = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| Error::Network(e.to_string()))?
        .get(url)
        .bearer_auth(access_token)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| Error::Auth(e.to_string()))?;
    let json: serde_json::Value = resp.json().await.map_err(|e| Error::Auth(e.to_string()))?;
    let email = json
        .get("email")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Auth("no email in userinfo response".into()))?;
    Ok(email.to_string())
}

struct CancelListener(Arc<AtomicBool>);
impl Drop for CancelListener {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Parse an origin-relative callback from tiny_http and validate state first.
fn callback_code(
    target: &str,
    expected_state: &str,
) -> std::result::Result<Option<String>, String> {
    let url = url::Url::parse("http://127.0.0.1")
        .unwrap()
        .join(target)
        .map_err(|e| e.to_string())?;
    if url.path() != "/callback" {
        return Ok(None);
    }
    let params: std::collections::HashMap<String, String> =
        url.query_pairs().into_owned().collect();
    if params.get("state").map(String::as_str) != Some(expected_state) {
        return Err("Invalid sign-in state. Return to Bang and try again.".into());
    }
    if params.contains_key("error") {
        return Err("Sign-in was declined. Return to Bang to try again.".into());
    }
    params
        .get("code")
        .filter(|code| !code.is_empty())
        .cloned()
        .map(Some)
        .ok_or_else(|| "Missing sign-in code. Return to Bang and try again.".into())
}

/// Block until a valid callback, cancellation, refusal or timeout.
fn wait_for_code(
    server: &tiny_http::Server,
    expected_state: &CsrfToken,
    cancelled: &AtomicBool,
) -> std::result::Result<String, String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Err("Sign-in cancelled".into());
        }
        if std::time::Instant::now() > deadline {
            return Err("Sign-in timed out. Try again.".into());
        }
        let request = match server.recv_timeout(Duration::from_millis(200)) {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(e) => return Err(e.to_string()),
        };
        let outcome = callback_code(request.url(), expected_state.secret());
        let (status, body) = match &outcome {
            Ok(Some(_)) => (
                200,
                "Bang received your sign-in. Return to Bang to finish checking your account.",
            ),
            Ok(None) => (404, "Not found"),
            Err(_) => (
                400,
                "Sign-in was not completed. Return to Bang and try again.",
            ),
        };
        let response = tiny_http::Response::from_string(body)
            .with_status_code(status)
            .with_header(
                tiny_http::Header::from_bytes(
                    &b"Content-Type"[..],
                    &b"text/plain; charset=utf-8"[..],
                )
                .unwrap(),
            );
        request.respond(response).map_err(|e| e.to_string())?;
        match outcome {
            Ok(Some(code)) => return Ok(code),
            Ok(None) => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_accepts_relative_requests_and_decodes_code() {
        assert_eq!(
            callback_code("/callback?code=a%2Bb&state=expected", "expected").unwrap(),
            Some("a+b".into())
        );
        assert_eq!(callback_code("/favicon.ico", "expected").unwrap(), None);
        for target in [
            "/callback?code=x",
            "/callback?code=x&state=wrong",
            "/callback?state=expected",
            "/callback?code=&state=expected",
            "/callback?error=access_denied&state=expected",
        ] {
            assert!(callback_code(target, "expected").is_err(), "{target}");
        }
    }
    #[test]
    fn microsoft_scopes_target_exchange_and_support_work_accounts() {
        let cfg = provider_config(OAuthProvider::Microsoft);
        assert!(cfg.auth_url.contains("/common/"));
        assert!(cfg
            .scopes
            .contains(&"https://outlook.office.com/IMAP.AccessAsUser.All"));
        assert!(cfg.scopes.contains(&"https://outlook.office.com/SMTP.Send"));
        assert!(cfg.scopes.contains(&"offline_access"));
    }
    #[test]
    fn loopback_listener_handles_real_http_relative_targets() {
        use std::io::{Read, Write};
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let worker = std::thread::spawn(move || {
            wait_for_code(
                &server,
                &CsrfToken::new("expected".into()),
                &AtomicBool::new(false),
            )
        });
        for (path, expected) in [
            ("/favicon.ico", "404"),
            ("/callback?code=local-code&state=expected", "200"),
        ] {
            let mut stream = std::net::TcpStream::connect(addr).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            write!(stream, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            assert!(response.lines().next().unwrap().contains(expected));
        }
        assert_eq!(worker.join().unwrap().unwrap(), "local-code");
    }

    #[test]
    fn cancelled_listener_returns_without_waiting_for_browser() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        drop(CancelListener(cancelled.clone()));
        assert!(
            wait_for_code(&server, &CsrfToken::new("state".into()), &cancelled)
                .unwrap_err()
                .contains("cancelled")
        );
    }
}
