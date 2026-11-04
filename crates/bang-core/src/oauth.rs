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
#[derive(Clone)]
pub struct OAuthTokens {
    /// Access token.
    pub access_token: String,
    /// Refresh token, if the provider issued one.
    pub refresh_token: Option<String>,
    /// Expiry as Unix timestamp (seconds), if known.
    pub expires_at: Option<i64>,
}

impl std::fmt::Debug for OAuthTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OAuthTokens([redacted])")
    }
}

/// Renew a grant using the registration that originally issued it.
pub(crate) async fn refresh(
    provider: OAuthProvider,
    registration: &crate::OAuthRegistration,
    refresh_token: &str,
    override_url: Option<&str>,
) -> Result<OAuthTokens> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Error::Network("Could not start token renewal.".into()))?;
    let mut params = vec![
        ("grant_type", "refresh_token"),
        ("client_id", registration.client_id.as_str()),
        ("refresh_token", refresh_token),
    ];
    if provider == OAuthProvider::Google {
        if let Some(parameter) = registration.desktop_parameter.as_deref() {
            params.push(("client_secret", parameter));
        }
    }
    let response = client
        .post(override_url.unwrap_or(provider_config(provider).token_url))
        .form(&params)
        .send()
        .await
        .map_err(|_| Error::Network("Token renewal is temporarily unavailable.".into()))?;
    let success = response.status().is_success();
    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|_| Error::Network("Invalid token renewal response.".into()))?;
    if !success {
        return Err(
            if matches!(
                json.get("error").and_then(|e| e.as_str()),
                Some("invalid_grant" | "invalid_client" | "unauthorized_client")
            ) {
                Error::ReconnectRequired
            } else {
                Error::Network("Token renewal is temporarily unavailable.".into())
            },
        );
    }
    let access_token = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::Network("Invalid token renewal response.".into()))?
        .to_owned();
    let seconds = json
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .filter(|v| *v > 0 && *v < 365 * 86400)
        .ok_or_else(|| Error::Network("Invalid token expiry.".into()))?;
    Ok(OAuthTokens {
        access_token,
        refresh_token: json
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_owned),
        expires_at: Some(chrono::Utc::now().timestamp() + seconds),
    })
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
            .add_extra_param("prompt", "consent select_account");
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
        .map_err(|e| Error::Auth(e.to_string()))??;

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
        .map_err(|_| Error::Auth("The provider could not complete sign-in. Try again.".into()))?;

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
        .map_err(|_| Error::Auth("Could not check the signed-in account.".into()))?;
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|_| Error::Auth("Could not check the signed-in account.".into()))?;
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

#[derive(Debug)]
enum CallbackError {
    Invalid,
    Declined,
}

/// Parse an origin-relative callback from tiny_http and validate state first.
fn callback_code(
    target: &str,
    expected_state: &str,
) -> std::result::Result<Option<String>, CallbackError> {
    if target.len() > 8192
        || !target.starts_with('/')
        || target.starts_with("//")
        || target.contains('\\')
    {
        return Ok(None);
    }
    let url = url::Url::parse("http://127.0.0.1")
        .unwrap()
        .join(target)
        .map_err(|_| CallbackError::Invalid)?;
    if url.host_str() != Some("127.0.0.1") || url.fragment().is_some() || url.path() != "/callback"
    {
        return Ok(None);
    }
    let mut params = std::collections::HashMap::new();
    for (key, value) in url.query_pairs().into_owned() {
        if params.insert(key.clone(), value).is_some()
            && matches!(key.as_str(), "state" | "code" | "error")
        {
            return Err(CallbackError::Invalid);
        }
    }
    if params.get("state").map(String::as_str) != Some(expected_state) {
        return Err(CallbackError::Invalid);
    }
    if params.contains_key("error") {
        if params.contains_key("code") {
            return Err(CallbackError::Invalid);
        }
        return Err(CallbackError::Declined);
    }
    params
        .get("code")
        .filter(|code| !code.is_empty())
        .cloned()
        .map(Some)
        .ok_or(CallbackError::Invalid)
}

/// Block until a valid callback, cancellation, refusal or timeout.
fn wait_for_code(
    server: &tiny_http::Server,
    expected_state: &CsrfToken,
    cancelled: &AtomicBool,
) -> Result<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        if std::time::Instant::now() > deadline {
            return Err(Error::Timeout);
        }
        let request = match server.recv_timeout(Duration::from_millis(200)) {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(_) => return Err(Error::Auth("Could not receive browser sign-in.".into())),
        };
        let outcome = if request.method() == &tiny_http::Method::Get
            && request.url().starts_with('/')
            && !request.url().starts_with("//")
        {
            callback_code(request.url(), expected_state.secret())
        } else {
            Ok(None)
        };
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
        // A disconnected foreign request must not cancel the user's sign-in.
        let _ = request.respond(response);
        match outcome {
            Ok(Some(code)) => return Ok(code),
            Ok(None) => {}
            Err(CallbackError::Declined) => return Err(Error::Declined),
            Err(_) => {}
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
            ("/callback?code=unrelated&state=wrong", "400"),
            ("/callback?code=one&code=two&state=expected", "400"),
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
                .to_string()
                .contains("cancelled")
        );
    }
}
